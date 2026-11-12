use super::remux_workflow::{batch_failure, tool_warning};
use super::*;

impl MkvoRuntime {
    pub(super) async fn load_propedit_template_impl(
        &self,
        request: PropEditTemplateRequest,
    ) -> RuntimeResult<PropEditTemplateResponse> {
        let files = self.resolve_rows(&request.files, &[]).await?;
        let template = request
            .template_path
            .as_deref()
            .and_then(|path| {
                files
                    .iter()
                    .find(|file| same_path(&file.path, Path::new(path)))
            })
            .or_else(|| files.first())
            .ok_or_else(|| RuntimeError::invalid("Select a scanned MKV template file first."))?;
        let audio_tracks = template
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .enumerate()
            .map(|(index, track)| prop_track_row(index, track))
            .collect();
        let subtitle_tracks = template
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Subtitle)
            .enumerate()
            .map(|(index, track)| prop_track_row(index, track))
            .collect();
        Ok(PropEditTemplateResponse {
            template_path: display_path(&template.path),
            template_file_name: template.file_name(),
            audio_tracks,
            subtitle_tracks,
            default_audio: selected_track_label(template, TrackKind::Audio, |track| track.default),
            forced_audio: selected_track_label(template, TrackKind::Audio, |track| track.forced),
            default_subtitle: selected_track_label(template, TrackKind::Subtitle, |track| {
                track.default
            }),
            forced_subtitle: selected_track_label(template, TrackKind::Subtitle, |track| {
                track.forced
            }),
        })
    }

    pub(super) async fn build_propedit_preview_impl(
        &self,
        request: PropEditPreviewRequest,
    ) -> RuntimeResult<PropEditPreviewResponse> {
        let plan = self.plan_propedit(&request).await?;
        self.persist_plan(&plan).await?;
        Ok(propedit_preview_response(&plan))
    }

    pub(super) async fn plan_propedit(
        &self,
        request: &PropEditPreviewRequest,
    ) -> RuntimeResult<PropertyEditPlan> {
        let files = self
            .resolve_rows(&request.files, &request.selected_paths)
            .await?;
        let settings = self.dependencies().settings.load().await?.0;
        let settings_fingerprint = stable_fingerprint(&settings)
            .map_err(|error| RuntimeError::internal(error.to_string()))?;
        let key = request
            .idempotency_key
            .clone()
            .unwrap_or_else(IdempotencyKey::generate);
        // mkvpropedit rewrites the file in place.
        let source_access = self
            .probe_source_access(&files, RequiredAccess::ReadWrite)
            .await;
        PropertyEditPlanner
            .build_plan(PropertyEditPlanRequest {
                files,
                source_access,
                container_title: title_edit(
                    request.container_title_mode,
                    &request.custom_container_title,
                ),
                video_track_name: title_edit(request.video_title_mode, &request.custom_video_title),
                track_edits: build_track_edits(request),
                authorized_roots: self.authorized_root_paths(),
                settings_fingerprint,
                tool_fingerprints: self.tool_fingerprints().await?,
                expires_in_seconds: 900,
                idempotency_key: key,
            })
            .map_err(Into::into)
    }

    pub(super) async fn start_propedit_apply_impl(
        &self,
        request: PropEditPreviewRequest,
    ) -> RuntimeResult<crate::compat::OperationJobResponse> {
        let (plan_id, fingerprint, key) = require_plan_fields(
            request.plan_id,
            request.plan_fingerprint,
            request.idempotency_key,
        )?;
        let expected_request = self
            .validate_plan_reference(plan_id, &fingerprint, &key)
            .await?;
        if let Some(existing) = self.dependencies().jobs.find_by_idempotency(&key).await? {
            validate_idempotent_job(&existing, plan_id, &expected_request)?;
            return Ok(crate::compat::OperationJobResponse::from_snapshot(
                &existing,
            ));
        }
        let plan: PropertyEditPlan = self.load_valid_plan(plan_id, &fingerprint, &key).await?;
        let total = u64::try_from(
            plan.payload
                .items
                .iter()
                .filter(|item| item.can_apply())
                .count(),
        )
        .unwrap_or(u64::MAX);
        let resources = plan.context.resources.clone();
        let runtime = self.clone();
        let fingerprint_for_task = fingerprint.clone();
        let key_for_task = key.clone();
        let accepted = self
            .jobs()
            .start(
                JobSpec {
                    kind: JobKind::PropertyEdit,
                    idempotency_key: key,
                    request_fingerprint: plan.metadata.request_fingerprint.clone(),
                    plan_id: Some(plan_id),
                    total,
                    resources,
                },
                move |context| async move {
                    let plan: PropertyEditPlan = runtime
                        .load_valid_plan(plan_id, &fingerprint_for_task, &key_for_task)
                        .await
                        .map_err(runtime_application_error)?;
                    runtime.execute_propedit_plan(&plan, &context).await
                },
            )
            .await?;
        let snapshot = self
            .jobs()
            .get(accepted.id)
            .await?
            .ok_or_else(|| RuntimeError::internal("new propedit job was not persisted"))?;
        Ok(crate::compat::OperationJobResponse::from_snapshot(
            &snapshot,
        ))
    }

    pub(super) async fn execute_propedit_plan(
        &self,
        plan: &PropertyEditPlan,
        context: &mkvo_application::JobContext,
    ) -> Result<JobCompletion, ApplicationError> {
        let runnable: Vec<_> = plan
            .payload
            .items
            .iter()
            .filter(|item| item.can_apply())
            .cloned()
            .collect();
        let mut journal = self
            .begin_journal(
                plan.metadata.id,
                &plan.metadata.idempotency_key,
                &plan.context.resources,
                runnable
                    .iter()
                    .map(|item| item.path.to_string_lossy().into_owned()),
            )
            .await?;
        let total = u64::try_from(runnable.len()).unwrap_or(u64::MAX);
        let workers = self
            .settings_service()
            .load()
            .await?
            .settings
            .workers
            .max_edit_workers;
        let mut executions = stream::iter(runnable.into_iter().enumerate())
            .map(|(index, item)| async move {
                let key = item.path.to_string_lossy().into_owned();
                let result = self
                    .execute_propedit_item(&item, context, index, total)
                    .await;
                (key, result)
            })
            .buffer_unordered(workers);
        let mut failures = Vec::new();
        while let Some((key, result)) = executions.next().await {
            match result {
                Ok(()) => {
                    journal.step = journal.step.saturating_add(1);
                    journal.complete_item(&key);
                    context.record_completed().await?;
                }
                Err(error) => {
                    journal.fail_item(&key, error.to_string());
                    context.record_failed().await?;
                    if !matches!(error, ApplicationError::Canceled) {
                        context
                            .log(JobLogLevel::Error, format!("{key}: {error}"))
                            .await?;
                    }
                    failures.push((key, error));
                }
            }
            journal.updated_utc = Utc::now();
            self.dependencies().journal.advance(&journal).await?;
        }
        if !failures.is_empty() {
            let error = batch_failure(failures, total);
            journal.status = mkvo_application::JournalStatus::Failed;
            journal.detail = Some(error.to_string());
            journal.updated_utc = Utc::now();
            let _ = self.dependencies().journal.advance(&journal).await;
            return Err(error);
        }
        journal.status = mkvo_application::JournalStatus::Completed;
        journal.detail = Some("property-edit plan completed".to_owned());
        journal.updated_utc = Utc::now();
        self.dependencies().journal.advance(&journal).await?;
        let mut response = propedit_preview_response(plan);
        response.status = "Track-properties operation completed".to_owned();
        self.append_log(
            "Properties",
            "Property-edit plan completed",
            &plan.metadata.id.to_string(),
        )
        .await
        .map_err(runtime_application_error)?;
        Ok(JobCompletion {
            result: Some(serde_json::to_value(response).map_err(|error| {
                ApplicationError::Internal(format!("propedit result serialization failed: {error}"))
            })?),
            message: Some("Track-properties operation completed".to_owned()),
        })
    }

    pub(super) async fn execute_propedit_item(
        &self,
        item: &mkvo_domain::PropertyEditPlanItem,
        context: &mkvo_application::JobContext,
        index: usize,
        total: u64,
    ) -> Result<(), ApplicationError> {
        context.ensure_not_canceled()?;
        self.revalidate_fingerprint(&item.source_fingerprint)
            .await
            .map_err(runtime_application_error)?;
        context
            .progress(
                u64::try_from(index).unwrap_or(u64::MAX),
                total,
                item.path.to_string_lossy(),
                0,
            )
            .await?;
        let invocation = self.propedit_invocation(item).await?;
        // mkvpropedit rewrites the file in place; a cancel waits for it rather than kill it mid-write.
        let execution = self
            .dependencies()
            .tool_executor
            .execute(&invocation, tokio_util::sync::CancellationToken::new())
            .await?;
        if let Some(warning) = tool_warning(&invocation.tool, &execution)? {
            context
                .log(
                    JobLogLevel::Warning,
                    format!("{}: {warning}", item.path.display()),
                )
                .await?;
        }
        // Refresh each successful file immediately. If a later file in the
        // batch fails, every tab still sees the edits that already landed.
        self.refresh_working_set(std::slice::from_ref(&item.path))
            .await;
        Ok(())
    }

    pub(super) async fn propedit_invocation(
        &self,
        item: &mkvo_domain::PropertyEditPlanItem,
    ) -> Result<ToolInvocation, ApplicationError> {
        let mut arguments = vec![item.path.to_string_lossy().into_owned()];
        for mutation in &item.mutations {
            append_propedit_arguments(&mut arguments, mutation);
        }
        Ok(ToolInvocation {
            tool: "mkvpropedit".to_owned(),
            executable: self.tool_path("mkvpropedit").await?,
            arguments,
            working_directory: item.path.parent().map(Path::to_path_buf),
            expected_outputs: Vec::new(),
        })
    }
}

#[cfg(test)]
mod execution_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use mkvo_application::{PortError, ToolExecutionResult, ToolExecutor};
    use mkvo_contracts::{JobSnapshot, JobStatus};
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;

    use super::super::remux_workflow::execution_tests::{Library, ScriptedTools, exited, library};
    use super::*;

    async fn start_title_edit(library: &Library, files: Vec<MediaFile>) -> mkvo_domain::JobId {
        let plan = PropertyEditPlanner
            .build_plan(PropertyEditPlanRequest {
                files,
                source_access: BTreeMap::new(),
                container_title: TextEdit::Set("Title".to_owned()),
                video_track_name: TextEdit::Keep,
                track_edits: Vec::new(),
                authorized_roots: Vec::new(),
                settings_fingerprint: "settings".to_owned(),
                tool_fingerprints: BTreeMap::new(),
                expires_in_seconds: 60,
                idempotency_key: IdempotencyKey::generate(),
            })
            .expect("propedit plan");
        library.runtime.persist_plan(&plan).await.expect("persist");
        library
            .start_job(JobKind::PropertyEdit, |runtime, context| async move {
                runtime.execute_propedit_plan(&plan, &context).await
            })
            .await
    }

    async fn run_title_edit(library: &Library, files: Vec<MediaFile>) -> JobSnapshot {
        let id = start_title_edit(library, files).await;
        library.finished_job(id).await
    }

    #[tokio::test]
    async fn an_edit_written_with_warnings_is_not_a_failure() {
        let library = library(ScriptedTools(|invocation: &ToolInvocation| {
            exited(
                invocation,
                1,
                "The file is being analyzed.\nWarning: No attachment matched the spec '5'.\nThe changes are written to the file.\nDone.\n",
            )
        }));
        let episode = library.media("Episode 01.mkv").await;

        let job = run_title_edit(&library, vec![episode]).await;

        assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);
        assert!(
            job.lines
                .iter()
                .any(|line| line.contains("No attachment matched the spec"))
        );
    }

    /// An edit that stays in flight until released, and stops early if it is told to.
    #[derive(Clone, Default)]
    struct HeldEdit {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl ToolExecutor for HeldEdit {
        async fn execute(
            &self,
            invocation: &ToolInvocation,
            cancel: CancellationToken,
        ) -> Result<ToolExecutionResult, PortError> {
            self.started.notify_one();
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(PortError::Canceled),
                () = self.release.notified() => Ok(exited(invocation, 0, "Done.\n")),
            }
        }
    }

    #[tokio::test]
    async fn canceling_lets_the_edit_in_flight_finish() {
        let tools = HeldEdit::default();
        let library = library(tools.clone());
        let episode = library.media("Episode 01.mkv").await;

        let id = start_title_edit(&library, vec![episode]).await;
        tools.started.notified().await;
        library.runtime.jobs().cancel(id).await.expect("cancel");
        tools.release.notify_one();
        let job = library.finished_job(id).await;

        assert_eq!(job.status, JobStatus::Canceled);
        assert_eq!((job.completed, job.failed), (1, 0));
    }

    #[tokio::test]
    async fn every_failed_file_is_reported() {
        let library = library(ScriptedTools(|invocation: &ToolInvocation| {
            if invocation.arguments[0].contains("Episode 02") {
                exited(invocation, 0, "Done.\n")
            } else {
                let failed = "Error: The file could not be opened for writing.\n";
                exited(invocation, 2, failed)
            }
        }));
        let mut files = Vec::new();
        for name in ["Episode 01.mkv", "Episode 02.mkv", "Episode 03.mkv"] {
            files.push(library.media(name).await);
        }

        let job = run_title_edit(&library, files).await;

        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.failed, 2);
        let error = job.error.expect("error");
        assert!(error.contains("2 of 3 files failed"), "{error}");
        assert!(
            error.contains("Episode 01.mkv: mkvpropedit failed"),
            "{error}"
        );
        assert!(
            error.contains("Episode 03.mkv: mkvpropedit failed"),
            "{error}"
        );
        assert!(!error.contains("Episode 02.mkv"), "{error}");
        let logged = job
            .lines
            .iter()
            .filter(|line| line.contains("could not be opened for writing"))
            .count();
        assert_eq!(logged, 2);
    }
}
