use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use mkvo_application::ScanOutcome;
use mkvo_contracts::{CurrentScanResponse, JobSnapshot, MediaFileRow, ScanSummary};
use mkvo_domain::{JobId, MediaFile};
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

    /// Move the authoritative working set and selection with completed renames.
    pub(super) fn apply_renames(&mut self, renames: &[(PathBuf, PathBuf)]) {
        for (source, target) in renames {
            let source_key = mkvo_application::paths::path_key(source);
            for file in &mut self.files {
                if mkvo_application::paths::path_key(&file.path) == source_key {
                    file.path = target.clone();
                }
            }
            if self.selected.remove(&source_key) {
                self.selected
                    .insert(mkvo_application::paths::path_key(target));
            }
        }
        self.files.sort_by(|left, right| left.path.cmp(&right.path));
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
