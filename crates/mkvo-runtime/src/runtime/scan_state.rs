use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use mkvo_application::ScanOutcome;
use mkvo_contracts::{CurrentScanResponse, JobSnapshot, MediaFileRow, ScanSummary};
use mkvo_domain::{JobId, MediaFile, same_path};
use serde::{Deserialize, Serialize};

use super::display_path;

/// Skipped paths kept in a scan's job result; the summary has the full count.
const MAX_STORED_SKIPS: usize = 500;

/// What a finished scan leaves in its job. The files are not here: they live
/// in the working set only, however large the library is.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ScanResultState {
    pub(super) skipped: Vec<String>,
    pub(super) summary: ScanSummary,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CurrentScanState {
    pub(super) updated_utc: Option<DateTime<Utc>>,
    /// The scan that produced `files`. Its status response serves them from
    /// here; once it loses them they are handed to its job instead.
    pub(super) scan_job: Option<JobId>,
    /// The latest scan that left the working set alone. Its files stay with
    /// its job, where a Library audit finds them by path.
    pub(super) overview_job: Option<JobId>,
    pub(super) files: Vec<MediaFile>,
    pub(super) summary: ScanSummary,
    /// Selected paths are normalized so equivalent Windows spellings match.
    pub(super) selected: BTreeSet<String>,
}

impl CurrentScanState {
    /// Replace the working set after a scan, selecting every file the first
    /// time a working set is populated. Later rescans preserve an explicit
    /// user selection, including a deliberate empty selection. Returns the
    /// files it replaced.
    pub(super) fn apply_scan(
        &mut self,
        files: Vec<MediaFile>,
        summary: ScanSummary,
    ) -> Vec<MediaFile> {
        let is_initial_scan = self.files.is_empty() && self.selected.is_empty();
        let replaced = std::mem::replace(&mut self.files, files);
        self.summary = summary;
        if is_initial_scan {
            self.selected = self
                .files
                .iter()
                .map(|file| mkvo_application::paths::path_key(&file.path))
                .collect();
        } else {
            self.reconcile_selection();
        }
        replaced
    }

    /// Narrow the working set to the files under the `wanted` keys, all
    /// selected, taking from `read` those it does not hold. Returns the files
    /// it replaced.
    pub(super) fn keep_only(
        &mut self,
        wanted: &BTreeSet<String>,
        read: Vec<MediaFile>,
    ) -> Vec<MediaFile> {
        let key = |file: &MediaFile| mkvo_application::paths::path_key(&file.path);
        // What the set holds now wins: an operation may have re-read it since `read` began.
        let held = self.files.iter().filter(|file| wanted.contains(&key(file)));
        let mut files: Vec<MediaFile> = held.cloned().collect();
        let mut taken: BTreeSet<String> = files.iter().map(key).collect();
        files.extend(read.into_iter().filter(|file| taken.insert(key(file))));
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let count = |extensions: &[&str]| {
            let matching = files.iter().filter(|file| {
                let extension = file.extension();
                extensions
                    .iter()
                    .any(|known| extension.eq_ignore_ascii_case(known))
            });
            matching.count() as u64
        };
        self.summary = ScanSummary {
            total: files.len() as u64,
            mkv: count(&["mkv"]),
            mp4: count(&["mp4", "m4v"]),
            ..ScanSummary::default()
        };
        self.selected = taken;
        std::mem::replace(&mut self.files, files)
    }

    /// Move the authoritative working set and selection with completed renames.
    pub(super) fn apply_renames(&mut self, renames: &[(PathBuf, PathBuf)]) {
        let mut positions: HashMap<String, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(position, file)| (mkvo_application::paths::path_key(&file.path), position))
            .collect();
        for (source, target) in renames {
            let source_key = mkvo_application::paths::path_key(source);
            let target_key = mkvo_application::paths::path_key(target);
            if let Some(position) = positions.remove(&source_key) {
                self.files[position].path = target.clone();
                positions.insert(target_key.clone(), position);
            }
            if self.selected.remove(&source_key) {
                self.selected.insert(target_key);
            }
        }
        self.files.sort_by(|left, right| left.path.cmp(&right.path));
    }

    /// Take re-read files into the working set: each replaces its entry, or
    /// is added when it has none.
    pub(super) fn merge(&mut self, files: Vec<MediaFile>) {
        let mut reordered = false;
        for file in files {
            // The set is in path order, so an unchanged path is found without a scan.
            let known = self
                .files
                .binary_search_by(|candidate| candidate.path.cmp(&file.path))
                .ok()
                .or_else(|| {
                    let mut files = self.files.iter();
                    files.position(|candidate| same_path(&candidate.path, &file.path))
                });
            match known {
                Some(position) => {
                    reordered |= self.files[position].path != file.path;
                    self.files[position] = file;
                }
                None => {
                    reordered = true;
                    self.files.push(file);
                }
            }
        }
        if reordered {
            self.files.sort_by(|left, right| left.path.cmp(&right.path));
            self.reconcile_selection();
        }
    }

    pub(super) fn reconcile_selection(&mut self) {
        if self.files.is_empty() {
            return;
        }
        let available: BTreeSet<String> = self
            .files
            .iter()
            .map(|file| mkvo_application::paths::path_key(&file.path))
            .collect();
        self.selected.retain(|path| available.contains(path));
    }

    pub(super) fn selected_display_paths(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|file| {
                self.selected
                    .contains(&mkvo_application::paths::path_key(&file.path))
            })
            .map(|file| display_path(&file.path))
            .collect()
    }
}

pub(super) fn scan_result_state(outcome: &ScanOutcome) -> ScanResultState {
    let mut skipped: Vec<String> = outcome
        .skipped
        .iter()
        .take(MAX_STORED_SKIPS)
        .map(|skip| format!("{}: {}", skip.path.display(), skip.reason))
        .collect();
    let hidden = outcome.skipped.len().saturating_sub(MAX_STORED_SKIPS);
    if hidden > 0 {
        skipped.push(format!("... and {hidden} more"));
    }
    ScanResultState {
        skipped,
        summary: outcome.summary,
    }
}

pub(super) fn current_scan_response(state: &CurrentScanState) -> CurrentScanResponse {
    CurrentScanResponse {
        updated_utc: state.updated_utc,
        files: state.files.iter().map(MediaFileRow::from).collect(),
        summary: state.summary,
        selected_paths: state.selected_display_paths(),
    }
}

pub(super) fn scan_state_from_snapshot(snapshot: &JobSnapshot) -> Option<ScanResultState> {
    // Read in place: a result stored by an older build also holds every file.
    ScanResultState::deserialize(snapshot.result.as_ref()?).ok()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::tests::media_at;
    use super::*;

    fn state_with(paths: &[&str]) -> CurrentScanState {
        CurrentScanState {
            files: paths.iter().map(|path| media_at(path)).collect(),
            ..CurrentScanState::default()
        }
    }

    fn paths(state: &CurrentScanState) -> Vec<String> {
        let paths = state.files.iter();
        paths.map(|file| display_path(&file.path)).collect()
    }

    #[test]
    fn a_batch_of_renames_may_reuse_a_name_it_just_freed() {
        let mut state = state_with(&["/media/a.mkv", "/media/b.mkv", "/media/z.mkv"]);
        state.selected = ["/media/a.mkv", "/media/b.mkv"]
            .map(|path| mkvo_application::paths::path_key(Path::new(path)))
            .into();

        state.apply_renames(&[
            (PathBuf::from("/media/b.mkv"), PathBuf::from("/media/c.mkv")),
            (PathBuf::from("/media/a.mkv"), PathBuf::from("/media/b.mkv")),
        ]);

        assert_eq!(
            paths(&state),
            ["/media/b.mkv", "/media/c.mkv", "/media/z.mkv"]
        );
        assert_eq!(
            state.selected_display_paths(),
            ["/media/b.mkv", "/media/c.mkv"]
        );
    }

    /// Reading the files a handoff names takes a while, and an operation may
    /// finish meanwhile: its re-read copy is the newer one.
    #[test]
    fn a_handoff_keeps_the_copies_the_working_set_holds_by_then() {
        let mut state = state_with(&["/media/a.mkv", "/media/other.mkv"]);
        state.files[0].fingerprint.size_bytes = 99;
        let wanted = ["/media/a.mkv", "/media/b.mkv"]
            .map(|path| mkvo_application::paths::path_key(Path::new(path)))
            .into();
        let read = ["/media/b.mkv", "/media/a.mkv", "/media/b.mkv"].map(media_at);

        let replaced = state.keep_only(&wanted, read.into());

        assert_eq!(paths(&state), ["/media/a.mkv", "/media/b.mkv"]);
        assert_eq!(state.files[0].fingerprint.size_bytes, 99);
        assert_eq!(state.selected_display_paths(), paths(&state));
        assert_eq!((state.summary.total, state.summary.mkv), (2, 2));
        assert_eq!(replaced.len(), 2);
    }

    #[test]
    fn re_read_files_replace_their_entry_and_new_ones_are_added_in_order() {
        let mut state = state_with(&["/media/a.mkv", "/media/c.mkv"]);
        let mut changed = media_at("/media/c.mkv");
        changed.fingerprint.size_bytes = 99;

        state.merge(vec![changed, media_at("/media/b.mkv")]);

        assert_eq!(
            paths(&state),
            ["/media/a.mkv", "/media/b.mkv", "/media/c.mkv"]
        );
        assert_eq!(state.files[2].fingerprint.size_bytes, 99);
    }
}
