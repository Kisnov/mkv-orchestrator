//! One canonical spelling for a path used as an identity.
//!
//! On Windows `fs::canonicalize` returns the extended-length `\\?\` form, but
//! filesystem-watch events, user input, and configured settings arrive in the
//! plain form. Two spellings of the same file must not become two cache rows or
//! a lookup that silently misses, so everything that keys on a path — cache
//! rows, plan resource claims, UI text — routes through here first.
//!
//! The prefix is kept where removing it would change meaning: a device path
//! such as `\\?\Volume{...}` has no plain equivalent, and long paths keep their
//! prefix when handed to an external tool, which is a separate concern from
//! identity.

use std::path::{Path, PathBuf};

/// Strip the Windows extended-length prefix when the result is still a valid
/// path. Non-Windows paths and device paths are returned unchanged.
#[must_use]
pub fn normalized_path_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return rest.to_owned();
    }
    text.into_owned()
}

/// [`normalized_path_text`] as a `PathBuf`.
#[must_use]
pub fn normalized_path(path: &Path) -> PathBuf {
    PathBuf::from(normalized_path_text(path))
}

/// Identity key for a path: one string per file, whatever its spelling.
///
/// Everywhere: the extended-length prefix is dropped, `\` and `/` are the same
/// separator and a trailing one is ignored, since stored plans and settings may
/// carry Windows spellings. Case is folded only on Windows and macOS, whose
/// filesystems ignore it; elsewhere `Ep.mkv` and `ep.mkv` are two files.
#[must_use]
pub fn path_key(path: &Path) -> String {
    let key = normalized_path_text(path).replace('\\', "/");
    let key = key.trim_end_matches('/');
    if cfg!(any(windows, target_os = "macos")) {
        key.to_lowercase()
    } else {
        key.to_owned()
    }
}

/// [`path_key`] with case folded on every platform: equal for two names that
/// would collide on a case-insensitive share, whatever the host is.
#[must_use]
pub fn case_folded_path_key(path: &Path) -> String {
    path_key(path).to_lowercase()
}

/// Compare two paths by identity rather than by spelling.
#[must_use]
pub fn same_path(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache is written from canonicalized scan paths but queried with raw
    /// watcher paths. Without one spelling, deletions never match a cache row
    /// and prune silently does nothing.
    #[test]
    fn canonical_and_raw_windows_paths_share_one_key() {
        assert_eq!(
            normalized_path_text(Path::new(r"\\?\C:\media\a.mkv")),
            normalized_path_text(Path::new(r"C:\media\a.mkv"))
        );
        assert!(same_path(
            Path::new(r"\\?\C:\media\a.mkv"),
            Path::new(r"C:\media\a.mkv")
        ));
    }

    #[test]
    fn a_key_ignores_the_prefix_the_separator_and_a_trailing_one() {
        assert_eq!(
            path_key(Path::new(r"\\?\C:\media\show\")),
            path_key(Path::new("C:/media/show"))
        );
        assert_eq!(
            path_key(Path::new(r"\\?\UNC\nas\media\a.mkv")),
            path_key(Path::new("//nas/media/a.mkv"))
        );
        assert_eq!(path_key(Path::new("/mnt/media/")), "/mnt/media");
    }

    /// Folding case where the filesystem keeps it made `Ep.mkv` and `ep.mkv`
    /// one file on a Linux NAS.
    #[test]
    fn case_is_part_of_the_key_only_where_the_filesystem_keeps_it() {
        let folds = cfg!(any(windows, target_os = "macos"));
        let upper = Path::new("/media/Show/Ep.mkv");
        let lower = Path::new("/media/show/ep.mkv");
        assert_eq!(path_key(upper) == path_key(lower), folds);
        assert_eq!(same_path(upper, lower), folds);
        assert_eq!(case_folded_path_key(upper), case_folded_path_key(lower));
    }

    #[test]
    fn unc_paths_normalize_to_their_plain_form() {
        assert_eq!(
            normalized_path_text(Path::new(r"\\?\UNC\nas\media\a.mkv")),
            r"\\nas\media\a.mkv"
        );
    }

    #[test]
    fn paths_without_a_plain_form_are_left_intact() {
        assert_eq!(
            normalized_path_text(Path::new(r"\\?\Volume{9f3a}\media")),
            r"\\?\Volume{9f3a}\media"
        );
        assert_eq!(
            normalized_path_text(Path::new("/mnt/media/a.mkv")),
            "/mnt/media/a.mkv"
        );
    }
}
