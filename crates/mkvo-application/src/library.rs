use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use mkvo_domain::{
    LibraryAudit, LibraryAuditGroup, LibraryAuditSummary, LibraryIssue, LibraryIssueKind,
    LibraryStandard, MediaFile, TrackKind, natural_compare,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct LibraryAuditService;

impl LibraryAuditService {
    #[must_use]
    pub fn build(
        &self,
        root: impl Into<PathBuf>,
        files: &[MediaFile],
        uncached_paths: &[PathBuf],
    ) -> LibraryAudit {
        let root = root.into();
        let mut grouped: BTreeMap<AuditGroupKey, Vec<&MediaFile>> = BTreeMap::new();
        for file in files {
            grouped
                .entry(group_key(&root, &file.path))
                .or_default()
                .push(file);
        }

        let mut groups = grouped
            .into_iter()
            .map(|(key, mut files)| {
                files.sort_by_key(|file| file.file_name().to_lowercase());
                build_group(&root, key, &files)
            })
            .collect::<Vec<_>>();
        groups.sort_by(|left, right| {
            left.show_name
                .to_lowercase()
                .cmp(&right.show_name.to_lowercase())
                .then_with(|| natural_compare(&left.season_folder, &right.season_folder))
        });
        let summary = LibraryAuditSummary {
            shows: groups
                .iter()
                .map(|group| group.show_name.to_lowercase())
                .collect::<BTreeSet<_>>()
                .len(),
            season_folders: groups.len(),
            files: files.len().saturating_add(uncached_paths.len()),
            issue_groups: groups.iter().filter(|group| group.has_issues()).count(),
            uncached_files: uncached_paths.len(),
        };
        LibraryAudit {
            root,
            groups,
            summary,
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AuditGroupKey {
    show_name: String,
    season_folder: String,
    relative_folder: PathBuf,
    sort_season: u32,
}

fn group_key(root: &Path, file: &Path) -> AuditGroupKey {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let directory = relative.parent().unwrap_or_else(|| Path::new(""));
    let segments: Vec<_> = directory
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if segments.is_empty() {
        return AuditGroupKey {
            show_name: "root".to_owned(),
            season_folder: "root".to_owned(),
            relative_folder: PathBuf::new(),
            sort_season: 0,
        };
    }
    if let Some((index, season, title_prefix)) =
        segments
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, value)| {
                parse_season_folder(value)
                    .map(|(season, title_prefix)| (index, season, title_prefix))
            })
    {
        let show_name = if index > 0 {
            segments[index - 1].clone()
        } else {
            title_prefix.unwrap_or_else(|| segments[0].clone())
        };
        let relative_folder = segments[..=index].iter().collect();
        return AuditGroupKey {
            show_name,
            season_folder: segments[index].clone(),
            relative_folder,
            sort_season: season,
        };
    }
    let folder = segments
        .last()
        .cloned()
        .unwrap_or_else(|| "root".to_owned());
    AuditGroupKey {
        show_name: folder,
        season_folder: "movie/single folder".to_owned(),
        relative_folder: directory.to_owned(),
        sort_season: 9_999,
    }
}

fn build_group(root: &Path, key: AuditGroupKey, files: &[&MediaFile]) -> LibraryAuditGroup {
    let videos: Vec<_> = files.iter().map(|file| video_signature(file)).collect();
    let audios: Vec<_> = files
        .iter()
        .map(|file| track_signature(file, TrackKind::Audio))
        .collect();
    let subtitles: Vec<_> = files
        .iter()
        .map(|file| track_signature(file, TrackKind::Subtitle))
        .collect();
    let standard = LibraryStandard {
        video: dominant(&videos),
        audio: dominant(&audios),
        subtitles: dominant(&subtitles),
    };
    let mut issues = Vec::new();
    let mut issue_paths = BTreeSet::new();
    for file in files {
        add_mismatch(
            &mut issues,
            &mut issue_paths,
            file,
            LibraryIssueKind::VideoMismatch,
            "video",
            &video_signature(file),
            &standard.video,
        );
        add_mismatch(
            &mut issues,
            &mut issue_paths,
            file,
            LibraryIssueKind::AudioMismatch,
            "audio",
            &track_signature(file, TrackKind::Audio),
            &standard.audio,
        );
        add_mismatch(
            &mut issues,
            &mut issue_paths,
            file,
            LibraryIssueKind::SubtitleMismatch,
            "subtitles",
            &track_signature(file, TrackKind::Subtitle),
            &standard.subtitles,
        );
    }
    add_episode_issues(&mut issues, &mut issue_paths, files);

    let template_file_path = files
        .iter()
        .find(|file| {
            video_signature(file) == standard.video
                && track_signature(file, TrackKind::Audio) == standard.audio
                && track_signature(file, TrackKind::Subtitle) == standard.subtitles
        })
        .or_else(|| files.first())
        .map(|file| file.path.clone());
    LibraryAuditGroup {
        watch_root: root.to_owned(),
        show_name: key.show_name,
        season_folder: key.season_folder,
        relative_folder: key.relative_folder,
        all_file_paths: files.iter().map(|file| file.path.clone()).collect(),
        issue_file_paths: issue_paths.into_iter().collect(),
        template_file_path,
        standard,
        issues,
    }
}

fn add_mismatch(
    issues: &mut Vec<LibraryIssue>,
    issue_paths: &mut BTreeSet<PathBuf>,
    file: &MediaFile,
    kind: LibraryIssueKind,
    label: &str,
    actual: &str,
    expected: &str,
) {
    if expected.eq_ignore_ascii_case("unknown") || actual.eq_ignore_ascii_case(expected) {
        return;
    }
    issue_paths.insert(file.path.clone());
    issues.push(LibraryIssue {
        kind,
        message: format!(
            "{}: {label} mismatch ({} vs {})",
            file.file_name(),
            display_signature(actual),
            display_signature(expected)
        ),
        path: Some(file.path.clone()),
        related_paths: Vec::new(),
    });
}

fn add_episode_issues(
    issues: &mut Vec<LibraryIssue>,
    issue_paths: &mut BTreeSet<PathBuf>,
    files: &[&MediaFile],
) {
    let numbered: Vec<_> = files
        .iter()
        .filter_map(|file| parse_episode(&file.file_name()).map(|parsed| (parsed, *file)))
        .collect();
    if numbered.len() < 2 {
        return;
    }
    let mut by_episode: BTreeMap<u32, Vec<PathBuf>> = BTreeMap::new();
    for (parsed, file) in &numbered {
        for episode in parsed.episode..=parsed.last_episode {
            by_episode
                .entry(episode)
                .or_default()
                .push(file.path.clone());
        }
    }
    let duplicate_numbers: Vec<_> = by_episode
        .iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(number, _)| *number)
        .collect();
    if !duplicate_numbers.is_empty() {
        let related_paths: Vec<_> = duplicate_numbers
            .iter()
            .flat_map(|number| by_episode[number].clone())
            .collect();
        issue_paths.extend(related_paths.iter().cloned());
        issues.push(LibraryIssue {
            kind: LibraryIssueKind::DuplicateEpisode,
            message: format!(
                "duplicate episode numbers: {}",
                duplicate_numbers
                    .iter()
                    .map(|number| format!("{number:02}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            path: None,
            related_paths,
        });
    }

    let numbers: Vec<_> = by_episode.keys().copied().collect();
    if numbers.len() < 3 {
        return;
    }
    let first = numbers[0];
    let last = *numbers.last().unwrap_or(&first);
    let known: BTreeSet<_> = numbers.into_iter().collect();
    let missing: Vec<_> = (first..=last)
        .filter(|number| !known.contains(number))
        .take(20)
        .collect();
    if !missing.is_empty() {
        issues.push(LibraryIssue {
            kind: LibraryIssueKind::PossibleMissingEpisode,
            message: format!(
                "possible missing episode numbers: {}",
                missing
                    .iter()
                    .map(|number| format!("{number:02}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            path: None,
            related_paths: Vec::new(),
        });
    }
}

fn video_signature(file: &MediaFile) -> String {
    file.tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map_or_else(
            || "unknown".to_owned(),
            |track| {
                [
                    track.resolution.map(|value| value.to_string()),
                    Some(clean(&track.codec, "unknown")),
                    track.bit_depth.map(|value| format!("{value}bit")),
                    track.hdr.clone().filter(|value| !value.trim().is_empty()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
            },
        )
}

fn track_signature(file: &MediaFile, kind: TrackKind) -> String {
    let mut tracks = file
        .tracks
        .iter()
        .filter(|track| track.kind == kind)
        .map(|track| {
            let mut value = format!(
                "{}:{}",
                track.language_or_undetermined(),
                clean(&track.codec, "unknown")
            );
            if track.forced {
                value.push_str(":forced");
            }
            if let Some(name) = track.name.as_deref().filter(|name| !name.trim().is_empty()) {
                value.push_str(&format!(" - \"{}\"", name.trim()));
            }
            value
        })
        .collect::<Vec<_>>();
    tracks.sort_by_key(|value| value.to_lowercase());
    if tracks.is_empty() {
        "none".to_owned()
    } else {
        tracks.join(", ")
    }
}

fn dominant(values: &[String]) -> String {
    let mut counts: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for value in values {
        let display = clean(value, "unknown");
        let entry = counts
            .entry(display.to_lowercase())
            .or_insert_with(|| (0, display));
        entry.0 += 1;
    }
    counts
        .into_values()
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
        .map_or_else(|| "unknown".to_owned(), |(_, value)| value)
}

fn clean(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn display_signature(value: &str) -> &str {
    if value.trim().is_empty() {
        "unknown"
    } else {
        value
    }
}

fn parse_season_folder(value: &str) -> Option<(u32, Option<String>)> {
    let compact: String = value
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if let Some(digits) = compact
        .strip_prefix("season")
        .or_else(|| compact.strip_prefix('s'))
        && !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return digits.parse().ok().map(|season| (season, None));
    }

    let lower = value.to_ascii_lowercase();
    for marker in ["season", "s"] {
        let Some(index) = lower.rfind(marker) else {
            continue;
        };
        let boundary_before = index == 0
            || lower[..index]
                .chars()
                .next_back()
                .is_some_and(|character| !character.is_ascii_alphanumeric());
        if !boundary_before {
            continue;
        }
        let digits = lower[index + marker.len()..].trim();
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let title = value[..index]
            .trim_end_matches(|character: char| {
                character.is_whitespace() || matches!(character, '-' | '_' | '.')
            })
            .trim();
        return digits
            .parse()
            .ok()
            .map(|season| (season, (!title.is_empty()).then(|| title.to_owned())));
    }
    None
}

/// Where a file sits in its series, as its name says.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParsedEpisode {
    /// Written with the episode, as in `S01E02` or `1x02`.
    pub season: Option<u32>,
    pub episode: u32,
    /// The last episode of a file that holds several; otherwise `episode`.
    pub last_episode: u32,
}

impl ParsedEpisode {
    fn single(season: Option<u32>, episode: u32) -> Self {
        Self {
            season,
            episode,
            last_episode: episode,
        }
    }
}

/// The number written at `start` and where it ends, if it has at most `max`
/// digits: a longer one is something else, and reading part of it is wrong.
fn number_at(bytes: &[u8], start: usize, max: usize) -> Option<(u32, usize)> {
    let length = bytes
        .get(start..)?
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if length == 0 || length > max {
        return None;
    }
    let digits = std::str::from_utf8(&bytes[start..start + length]).ok()?;
    Some((digits.parse().ok()?, start + length))
}

fn starts_word(bytes: &[u8], index: usize) -> bool {
    index == 0 || !bytes[index - 1].is_ascii_alphanumeric()
}

fn ends_word(bytes: &[u8], index: usize) -> bool {
    bytes
        .get(index)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric())
}

/// A number that is far more often a year or a video height than an episode.
fn is_year_or_resolution(number: u32) -> bool {
    (1900..=2099).contains(&number)
        || matches!(
            number,
            360 | 480 | 540 | 576 | 720 | 1080 | 1440 | 2160 | 4320
        )
}

/// `S01E02`, and with it `S01E01E02`, `S01E01-E03` and `S01E01-02`.
fn season_episode_marker(bytes: &[u8]) -> Option<ParsedEpisode> {
    (0..bytes.len()).find_map(|index| {
        // Also inside a word: `ShowS01E01` is a name people do write.
        if bytes[index] != b's' {
            return None;
        }
        let (season, cursor) = number_at(bytes, index + 1, 4)?;
        if bytes.get(cursor) != Some(&b'e') {
            return None;
        }
        let (episode, mut cursor) = number_at(bytes, cursor + 1, 4)?;
        let mut last_episode = episode;
        loop {
            let rest = &bytes[cursor..];
            let marked = rest.starts_with(b"-e") || rest.starts_with(b"e");
            let skipped = if rest.starts_with(b"-e") { 2 } else { 1 };
            if !marked && !rest.starts_with(b"-") {
                break;
            }
            let Some((next, end)) = number_at(bytes, cursor + skipped, 4) else {
                break;
            };
            // A bare `-02` has to stand alone and look like an episode.
            if next <= last_episode
                || !marked && (!ends_word(bytes, end) || is_year_or_resolution(next))
            {
                break;
            }
            last_episode = next;
            cursor = end;
        }
        Some(ParsedEpisode {
            season: Some(season),
            episode,
            last_episode,
        })
    })
}

/// `1x02`, but not the `1920x1080` of a resolution.
fn season_x_episode(bytes: &[u8]) -> Option<ParsedEpisode> {
    (0..bytes.len()).find_map(|index| {
        if !starts_word(bytes, index) {
            return None;
        }
        let (season, cursor) = number_at(bytes, index, 2)?;
        if bytes.get(cursor) != Some(&b'x') {
            return None;
        }
        let (episode, end) = number_at(bytes, cursor + 1, 4)?;
        (end - cursor > 2 && ends_word(bytes, end))
            .then(|| ParsedEpisode::single(Some(season), episode))
    })
}

/// Parse the episode a media filename states with a marker: `S01E02`, `1x02`,
/// `Episode 7`, `E07`, or a number alone in brackets.
#[must_use]
pub fn parse_episode(file_name: &str) -> Option<ParsedEpisode> {
    let lower = file_name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    if let Some(parsed) = season_episode_marker(bytes).or_else(|| season_x_episode(bytes)) {
        return Some(parsed);
    }
    for marker in ["episode", "ep", "e"] {
        let mut offset = 0;
        while let Some(found) = lower[offset..].find(marker) {
            let start = offset + found;
            let mut cursor = start + marker.len();
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if starts_word(bytes, start)
                && let Some((episode, end)) = number_at(bytes, cursor, 4)
                && ends_word(bytes, end)
            {
                return Some(ParsedEpisode::single(None, episode));
            }
            offset = start + marker.len();
        }
    }
    // Anime release names commonly put the episode in its own numeric bracket,
    // for example `[VCB-Studio] Show [01][Ma10p_1080p].mkv`. This is checked
    // after explicit SxxExx / Episode markers so a deliberate marker always
    // wins. The whole bracket has to be a number, and not one that reads as a
    // year or a resolution.
    lower
        .split('[')
        .skip(1)
        .filter_map(|bracket| bracket.split_once(']'))
        .find_map(|(content, _)| {
            let (number, end) = number_at(content.as_bytes(), 0, 4)?;
            (end == content.len() && !is_year_or_resolution(number))
                .then(|| ParsedEpisode::single(None, number))
        })
}

/// [`parse_episode`], or the release form `Title - 01`, where only the spaced
/// dash marks the number; `Title S2 - 03` also names the season.
///
/// Rename accepts this looser form. The library audit does not, so that it
/// never takes an unrelated number for an episode.
#[must_use]
pub fn parse_release_episode(file_name: &str) -> Option<ParsedEpisode> {
    if let Some(parsed) = parse_episode(file_name) {
        return Some(parsed);
    }
    let stem = Path::new(file_name).file_stem()?.to_string_lossy();
    let stem = stem.to_ascii_lowercase().replace('_', " ");
    let bytes = stem.as_bytes();
    stem.match_indices(" - ").find_map(|(dash, _)| {
        let (episode, mut end) = number_at(bytes, dash + 3, 4)?;
        // `01v2` is a second release of episode 1.
        if bytes.get(end) == Some(&b'v')
            && let Some((_, version_end)) = number_at(bytes, end + 1, 1)
        {
            end = version_end;
        }
        let stands_alone = bytes
            .get(end)
            .is_none_or(|byte| matches!(byte, b' ' | b'[' | b'('));
        // A resolution is not ruled out here: episode 720 of a long show is real.
        (stands_alone && !(1900..=2099).contains(&episode))
            .then(|| ParsedEpisode::single(trailing_season(&stem[..dash]), episode))
    })
}

/// The season a title ends with: `Title S2`, `Title Season 2`, `Title 2nd Season`.
fn trailing_season(title: &str) -> Option<u32> {
    let mut words = title.split_whitespace().rev();
    let last = words.next()?;
    let digits = if last == "season" {
        words
            .next()?
            .trim_end_matches(|character: char| character.is_ascii_alphabetic())
    } else if words.next() == Some("season") {
        last
    } else {
        last.strip_prefix('s')?
    };
    ((1..=2).contains(&digits.len())).then(|| digits.parse().ok())?
}

#[must_use]
pub fn parse_episode_number(file_name: &str) -> Option<u32> {
    parse_episode(file_name).map(|parsed| parsed.episode)
}

/// Parse the season and episode a media filename states together, as in
/// `S01E02` or `1x02`.
///
/// Keeping the season alongside the episode prevents a multi-season rename
/// from matching every `E01` to the first season returned by the provider.
#[must_use]
pub fn parse_season_episode(file_name: &str) -> Option<(u32, u32)> {
    let parsed = parse_episode(file_name)?;
    Some((parsed.season?, parsed.episode))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use mkvo_domain::{
        ContainerMetadata, FileFingerprint, MediaStatus, MediaTrack, VideoResolution,
    };

    use super::*;
    use serde_json::Value;

    fn media(path: &str, audio_language: &str) -> MediaFile {
        MediaFile {
            path: PathBuf::from(path),
            original_file_name: None,
            watch_root: None,
            relative_path: None,
            fingerprint: FileFingerprint {
                path: PathBuf::from(path),
                size_bytes: 1,
                modified_at: Utc::now(),
                quick_hash: None,
            },
            container: ContainerMetadata::default(),
            tracks: vec![
                track(0, TrackKind::Video, "und", "AVC"),
                track(1, TrackKind::Audio, audio_language, "AAC"),
            ],
            attachments: Vec::new(),
            episode: None,
            provider_match: None,
            status: MediaStatus::Ready,
        }
    }

    fn track(id: u64, kind: TrackKind, language: &str, codec: &str) -> MediaTrack {
        MediaTrack {
            mkvmerge_id: id,
            propedit_track_number: u32::try_from(id + 1).unwrap(),
            kind,
            codec: codec.to_owned(),
            codec_id: None,
            language: Some(language.to_owned()),
            name: None,
            resolution: (kind == TrackKind::Video).then_some(VideoResolution {
                width: 1_920,
                height: 1_080,
            }),
            bit_depth: None,
            hdr: None,
            channels: None,
            sampling_frequency_hz: None,
            default: true,
            forced: false,
            enabled: true,
        }
    }

    #[test]
    fn finds_common_episode_patterns() {
        assert_eq!(parse_episode_number("Show.S02E013.mkv"), Some(13));
        assert_eq!(parse_season_episode("Show.S06E01.mkv"), Some((6, 1)));
        assert_eq!(parse_season_episode("Show - Episode 007.mkv"), None);
        assert_eq!(parse_episode_number("Show - Episode 007.mkv"), Some(7));
        assert_eq!(
            parse_episode_number("[VCB-Studio] 7th Time Loop [01][Ma10p_1080p][x265_flac].mkv"),
            Some(1)
        );
        assert_eq!(
            parse_episode_number("[VCB-Studio] Show [Ma10p_1080p][A1B2C3D4].mkv"),
            None
        );
    }

    fn parsed(season: Option<u32>, episode: u32, last_episode: u32) -> Option<ParsedEpisode> {
        Some(ParsedEpisode {
            season,
            episode,
            last_episode,
        })
    }

    #[test]
    fn episode_markers_are_read_whole_and_only_where_they_are_markers() {
        for (name, expected) in [
            ("Show.S02E013.mkv", parsed(Some(2), 13, 13)),
            ("Show - S01E05 - Name.mkv", parsed(Some(1), 5, 5)),
            ("Show S01E01v2.mkv", parsed(Some(1), 1, 1)),
            ("Show S00E03.mkv", parsed(Some(0), 3, 3)),
            ("ShowS01E01.mkv", parsed(Some(1), 1, 1)),
            // A marked number may be any episode; only a bare one is read as a year.
            ("Show S01E1999.mkv", parsed(Some(1), 1999, 1999)),
            ("Show E2000.mkv", parsed(None, 2000, 2000)),
            // A double episode used to lose its second half.
            ("Show S01E01E02.mkv", parsed(Some(1), 1, 2)),
            ("Show S01E01-E03.mkv", parsed(Some(1), 1, 3)),
            ("Show S01E01-02.mkv", parsed(Some(1), 1, 2)),
            ("Show S01E05-2024.mkv", parsed(Some(1), 5, 5)),
            ("Show S01E05-1080p.mkv", parsed(Some(1), 5, 5)),
            // Four digits used to be cut to three: 1071 became 107.
            ("One Piece S01E1071.mkv", parsed(Some(1), 1071, 1071)),
            ("Show S01E12345.mkv", None),
            ("Show E1234.mkv", parsed(None, 1234, 1234)),
            ("Show - 1x02.mkv", parsed(Some(1), 2, 2)),
            ("Show 12x103.mkv", parsed(Some(12), 103, 103)),
            ("Show 1920x1080.mkv", None),
            ("Show x264.mkv", None),
            ("Show - Episode 007.mkv", parsed(None, 7, 7)),
            ("Show Ep 05 [720p].mkv", parsed(None, 5, 5)),
            (
                "[VCB-Studio] 7th Time Loop [01][Ma10p_1080p][x265_flac].mkv",
                parsed(None, 1, 1),
            ),
            ("One Piece [1071][1080p].mkv", parsed(None, 1071, 1071)),
            ("[VCB-Studio] Show [Ma10p_1080p][A1B2C3D4].mkv", None),
            // A resolution or a year in brackets is not an episode.
            ("Show [720].mkv", None),
            ("Show [1080].mkv", None),
            ("Show [2024].mkv", None),
            // Numbers that belong to the title.
            ("86 EIGHTY-SIX.mkv", None),
            ("Mob Psycho 100.mkv", None),
            ("Title - OVA [1080p].mkv", None),
        ] {
            assert_eq!(parse_episode(name), expected, "{name}");
        }
    }

    #[test]
    fn release_names_mark_the_episode_with_a_spaced_dash() {
        for (name, expected) in [
            (
                "[SubsPlease] Sousou no Frieren - 01 (1080p) [ABCD1234].mkv",
                parsed(None, 1, 1),
            ),
            (
                "[Erai-raws] Title - 01 [1080p][HEVC].mkv",
                parsed(None, 1, 1),
            ),
            ("[Group] Title - 01v2 [720p].mkv", parsed(None, 1, 1)),
            ("[HorribleSubs]_Title_-_07_[720p].mkv", parsed(None, 7, 7)),
            (
                "[Anime Time] My Hero Academia - 55.mkv",
                parsed(None, 55, 55),
            ),
            ("Title - 05 - 10 Years Later.mkv", parsed(None, 5, 5)),
            ("Title (2019) - 05 [BD 1080p].mkv", parsed(None, 5, 5)),
            // A season written before the dash.
            ("Title S2 - 03.mkv", parsed(Some(2), 3, 3)),
            ("Title Season 2 - 03 [1080p].mkv", parsed(Some(2), 3, 3)),
            ("Title 2nd Season - 03 [1080p].mkv", parsed(Some(2), 3, 3)),
            // Explicit markers still win.
            ("Title - 1x02.mkv", parsed(Some(1), 2, 2)),
            ("Title - S01E01-E03 [1080p].mkv", parsed(Some(1), 1, 3)),
            // Long-running shows.
            (
                "[Judas] One Piece - 1071 [1080p][HEVC x265 10bit].mkv",
                parsed(None, 1071, 1071),
            ),
            (
                "[Group] One Piece - 720 (1080p).mkv",
                parsed(None, 720, 720),
            ),
            ("Title - 100.mkv", parsed(None, 100, 100)),
            // Numbers in the title are not the episode.
            ("[Group] 86 EIGHTY-SIX - 05 [1080p].mkv", parsed(None, 5, 5)),
            ("Mob Psycho 100 - 07.mkv", parsed(None, 7, 7)),
            ("Mob Psycho 100 III - 12 [1080p].mkv", parsed(None, 12, 12)),
            ("Mob Psycho 100 [1080p].mkv", None),
            // A year, a half episode, a special and a number too long to be one.
            ("Title - 2024 [1080p].mkv", None),
            ("Title - 12.5 [1080p].mkv", None),
            ("[Group] Title - OVA 2 [BD].mkv", None),
            ("Example Show - 20240.mkv", None),
            ("Example Show 55.mkv", None),
        ] {
            assert_eq!(parse_release_episode(name), expected, "{name}");
        }
    }

    /// The second half of a double episode was reported as missing.
    #[test]
    fn a_double_episode_file_covers_both_of_its_episodes() {
        let files = vec![
            media("root/Show/Season 1/Show S01E01E02.mkv", "eng"),
            media("root/Show/Season 1/Show S01E03.mkv", "eng"),
            media("root/Show/Season 1/Show S01E04.mkv", "eng"),
        ];
        let audit = LibraryAuditService.build("root", &files, &[]);
        assert_eq!(audit.groups[0].issues, []);
    }

    #[test]
    fn audit_reports_track_mismatch_and_missing_episode() {
        let files = vec![
            media("root/Show/Season 1/Show S01E01.mkv", "eng"),
            media("root/Show/Season 1/Show S01E03.mkv", "eng"),
            media("root/Show/Season 1/Show S01E04.mkv", "jpn"),
        ];
        let audit = LibraryAuditService.build("root", &files, &[]);
        assert_eq!(audit.groups.len(), 1);
        assert_eq!(audit.summary.issue_groups, 1);
        assert!(
            audit.groups[0]
                .issues
                .iter()
                .any(|issue| issue.kind == LibraryIssueKind::AudioMismatch)
        );
        assert!(
            audit.groups[0]
                .issues
                .iter()
                .any(|issue| issue.kind == LibraryIssueKind::PossibleMissingEpisode)
        );
    }

    #[test]
    fn named_season_folders_stay_grouped_under_the_parent_series() {
        let files = vec![
            media(
                "root/Roseanne (1988)/Roseanne - Season 01/Roseanne S01E01.mkv",
                "eng",
            ),
            media(
                "root/Roseanne (1988)/Roseanne - Season 02/Roseanne S02E01.mkv",
                "eng",
            ),
        ];
        let audit = LibraryAuditService.build("root", &files, &[]);

        assert_eq!(audit.summary.shows, 1);
        assert_eq!(audit.groups.len(), 2);
        assert!(
            audit
                .groups
                .iter()
                .all(|group| group.show_name == "Roseanne (1988)")
        );
        assert_eq!(audit.groups[0].season_folder, "Roseanne - Season 01");
        assert_eq!(audit.groups[1].season_folder, "Roseanne - Season 02");
    }

    fn fixture_track(id: u64, value: &Value) -> MediaTrack {
        let kind = match value["type"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "video" => TrackKind::Video,
            "audio" => TrackKind::Audio,
            "subtitle" | "subtitles" => TrackKind::Subtitle,
            _ => TrackKind::Other,
        };
        MediaTrack {
            mkvmerge_id: id,
            propedit_track_number: u32::try_from(id + 1).expect("track number fits u32"),
            kind,
            codec: value["codec"].as_str().unwrap_or_default().to_owned(),
            codec_id: None,
            language: value["language"].as_str().map(str::to_owned),
            name: value["name"].as_str().map(str::to_owned),
            resolution: None,
            bit_depth: None,
            hdr: None,
            channels: None,
            sampling_frequency_hz: None,
            default: value["default"].as_bool().unwrap_or(false),
            forced: value["forced"].as_bool().unwrap_or(false),
            enabled: true,
        }
    }

    fn fixture_media(value: &Value) -> MediaFile {
        let path = value["filePath"].as_str().expect("file path");
        let metadata = &value["metadata"];
        let resolution = metadata["resolution"]
            .as_str()
            .and_then(|value| value.split_once('x'))
            .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
            .map(|(width, height)| VideoResolution { width, height });
        let bit_depth = metadata["bitDepth"]
            .as_str()
            .and_then(|value| value.trim_end_matches("bit").parse::<u8>().ok());
        let mut tracks = vec![MediaTrack {
            mkvmerge_id: 0,
            propedit_track_number: 1,
            kind: TrackKind::Video,
            codec: metadata["codec"].as_str().unwrap_or_default().to_owned(),
            codec_id: None,
            language: Some("und".to_owned()),
            name: None,
            resolution,
            bit_depth,
            hdr: metadata["hdr"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            channels: None,
            sampling_frequency_hz: None,
            default: true,
            forced: false,
            enabled: true,
        }];
        tracks.extend(
            value["tracks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|track| fixture_track(track["mkvMergeId"].as_u64().unwrap(), track)),
        );
        MediaFile {
            path: PathBuf::from(path),
            original_file_name: None,
            watch_root: None,
            relative_path: None,
            fingerprint: FileFingerprint {
                path: PathBuf::from(path),
                size_bytes: 1,
                modified_at: Utc::now(),
                quick_hash: None,
            },
            container: ContainerMetadata::default(),
            tracks,
            attachments: Vec::new(),
            episode: None,
            provider_match: None,
            status: MediaStatus::Ready,
        }
    }

    fn portable(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn executes_library_audit_fixture_case() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/parity-fixtures/library-audit.json"
        ))
        .expect("library fixture JSON");
        let cases = fixture["cases"].as_array().expect("fixture cases");
        assert_eq!(cases.len(), 1, "fixture case count changed");
        let case = &cases[0];
        let input = &case["input"];
        let cached = input["cache"]
            .as_array()
            .expect("cache")
            .iter()
            .map(fixture_media)
            .collect::<Vec<_>>();
        let cached_paths = cached
            .iter()
            .map(|file| portable(&file.path))
            .collect::<BTreeSet<_>>();
        let uncached = input["enumeratedFiles"]
            .as_array()
            .expect("enumerated files")
            .iter()
            .filter_map(Value::as_str)
            .filter(|path| !cached_paths.contains(*path))
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let audit = LibraryAuditService.build(
            input["watchRoot"].as_str().expect("watch root"),
            &cached,
            &uncached,
        );
        let expected = &case["expected"];
        assert_eq!(
            audit.summary.shows,
            expected["summary"]["shows"].as_u64().unwrap() as usize
        );
        assert_eq!(
            audit.summary.season_folders,
            expected["summary"]["seasonFolders"].as_u64().unwrap() as usize
        );
        assert_eq!(
            audit.summary.files,
            expected["summary"]["files"].as_u64().unwrap() as usize
        );
        assert_eq!(
            audit.summary.issue_groups,
            expected["summary"]["issueGroups"].as_u64().unwrap() as usize
        );
        assert_eq!(
            audit.summary.uncached_files,
            expected["summary"]["uncachedFiles"].as_u64().unwrap() as usize
        );
        let expected_groups = expected["groups"].as_array().expect("expected groups");
        assert_eq!(audit.groups.len(), expected_groups.len());
        for (actual, expected) in audit.groups.iter().zip(expected_groups) {
            assert_eq!(actual.show_name, expected["showName"].as_str().unwrap());
            assert_eq!(
                actual.season_folder,
                expected["seasonFolder"].as_str().unwrap()
            );
            assert_eq!(
                portable(&actual.relative_folder),
                expected["relativeFolder"].as_str().unwrap()
            );
            assert_eq!(
                actual.standard.video,
                expected["standardVideo"].as_str().unwrap()
            );
            assert_eq!(
                actual.standard.audio,
                expected["standardAudio"].as_str().unwrap()
            );
            assert_eq!(
                actual.standard.subtitles,
                expected["standardSubtitles"].as_str().unwrap()
            );
            assert_eq!(
                actual
                    .issues
                    .iter()
                    .map(|issue| issue.message.as_str())
                    .collect::<Vec<_>>(),
                expected["issues"]
                    .as_array()
                    .expect("issues")
                    .iter()
                    .map(|issue| issue.as_str().unwrap())
                    .collect::<Vec<_>>()
            );
        }
    }
}
