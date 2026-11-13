use super::*;
use base64::Engine as _;

impl MkvoRuntime {
    pub(super) async fn build_mux_preview_impl(
        &self,
        request: MuxPreviewRequest,
    ) -> RuntimeResult<MuxPreviewResponse> {
        let plan = self.plan_mux(&request).await?;
        self.persist_plan(&plan).await?;
        Ok(mux_preview_response(&plan))
    }

    pub(super) async fn plan_mux(&self, request: &MuxPreviewRequest) -> RuntimeResult<RemuxPlan> {
        let files = self
            .resolve_rows(&request.files, &request.selected_paths)
            .await?;
        let reorder_template = if request.reorder_tracks {
            let template_path = PathBuf::from(&request.reorder_template_path);
            Some(
                files
                    .iter()
                    .find(|file| same_path(&file.path, &template_path))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::invalid(
                            "Reorder template must be one of the selected MKV files.",
                        )
                    })?,
            )
        } else {
            None
        };
        let mode = if request.convert_mp4_to_mkv {
            RemuxMode::ConvertToMkv
        } else if request.extract_subtitles {
            RemuxMode::ExtractSubtitles
        } else if request.mux_matching_external_subtitles
            || !request.manual_subtitle_selections.is_empty()
        {
            RemuxMode::MuxSubtitles
        } else {
            RemuxMode::Remux
        };
        let mut existing_paths = self.current_existing_paths().await;
        let mut external_subtitles =
            if mode == RemuxMode::MuxSubtitles && request.mux_matching_external_subtitles {
                discover_external_subtitles(
                    &files,
                    &request.external_subtitle_formats,
                    &request.external_subtitle_language,
                    request.external_subtitle_track_name.as_deref(),
                )
                .await
            } else {
                BTreeMap::new()
            };
        let allowed_subtitle_formats = split_strings(&request.external_subtitle_formats);
        let mut manual_subtitle_paths = BTreeSet::new();
        for selection in &request.manual_subtitle_selections {
            let target = PathBuf::from(&selection.target_path);
            let Some(file) = files.iter().find(|file| same_path(&file.path, &target)) else {
                return Err(RuntimeError::invalid(format!(
                    "Manual subtitle target is not a selected MKV: {}",
                    selection.target_path
                )));
            };
            let subtitle_path = PathBuf::from(&selection.subtitle_path);
            let extension = subtitle_path.extension().map_or_else(String::new, |value| {
                value.to_string_lossy().to_ascii_lowercase()
            });
            if !allowed_subtitle_formats.contains(&extension) {
                return Err(RuntimeError::invalid(format!(
                    "Unsupported manual subtitle format: {}",
                    selection.subtitle_path
                )));
            }
            if !tokio::fs::metadata(&subtitle_path)
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                return Err(RuntimeError::not_found(format!(
                    "Manual subtitle file was not found: {}",
                    selection.subtitle_path
                )));
            }
            let subtitle_key = path_key(&subtitle_path.to_string_lossy());
            manual_subtitle_paths.insert(subtitle_key.clone());
            let subtitles = external_subtitles.entry(file.path.clone()).or_default();
            if subtitles
                .iter()
                .any(|subtitle| path_key(&subtitle.path.to_string_lossy()) == subtitle_key)
            {
                continue;
            }
            let forced = subtitle_path.file_stem().is_some_and(|stem| {
                stem.to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("forced")
            });
            subtitles.push(ExternalSubtitle {
                path: subtitle_path,
                language: request.external_subtitle_language.trim().to_owned(),
                name: None,
                default: false,
                forced,
            });
        }
        if request.skip_mux_if_subtitle_already_exists {
            for file in &files {
                if let Some(subtitles) = external_subtitles.get_mut(&file.path) {
                    subtitles.retain(|subtitle| {
                        manual_subtitle_paths.contains(&path_key(&subtitle.path.to_string_lossy()))
                            || !file.tracks.iter().any(|track| {
                                track.kind == TrackKind::Subtitle
                                    && track
                                        .language_or_undetermined()
                                        .eq_ignore_ascii_case(&subtitle.language)
                                    && subtitle_codec_matches(
                                        &track.codec,
                                        track.codec_id.as_deref(),
                                        &subtitle.path,
                                    )
                                    && subtitle_names_match(
                                        track.name.as_deref(),
                                        subtitle.name.as_deref(),
                                    )
                            })
                    });
                }
                if external_subtitles
                    .get(&file.path)
                    .is_some_and(Vec::is_empty)
                {
                    external_subtitles.remove(&file.path);
                }
            }
        }
        let extractions = if mode == RemuxMode::ExtractSubtitles {
            build_extractions(
                &files,
                &request.extract_subtitle_languages,
                request.extract_overwrite_existing_files,
            )
        } else {
            BTreeMap::new()
        };
        existing_paths.extend(
            external_subtitles
                .values()
                .flatten()
                .map(|subtitle| subtitle.path.clone()),
        );
        if !request.extract_overwrite_existing_files {
            existing_paths.extend(
                extractions
                    .values()
                    .flatten()
                    .filter(|track| track.output.exists())
                    .map(|track| track.output.clone()),
            );
        }
        let settings = self.dependencies().settings.load().await?.0;
        let settings_fingerprint = stable_fingerprint(&settings)
            .map_err(|error| RuntimeError::internal(error.to_string()))?;
        let remove_track_ids = if request.remove_unwanted_track_ids {
            split_u64s(&request.remove_track_ids_text)
        } else {
            BTreeSet::new()
        };
        let key = request
            .idempotency_key
            .clone()
            .unwrap_or_else(IdempotencyKey::generate);
        // Extraction only reads the source; every other mode rewrites it.
        let required = if mode == RemuxMode::ExtractSubtitles
            || (request.preserve_original
                && matches!(mode, RemuxMode::Remux | RemuxMode::MuxSubtitles))
        {
            RequiredAccess::Read
        } else {
            RequiredAccess::ReadWrite
        };
        let source_access = self.probe_source_access(&files, required).await;
        let base = RemuxPlanner
            .build_plan(RemuxPlanRequest {
                mode,
                files: files.clone(),
                source_access,
                options: RemuxOptions {
                    filter_audio_languages: request.remove_unwanted_audio_languages,
                    keep_audio_languages: split_strings(&request.keep_audio_languages),
                    filter_subtitle_languages: request.remove_unwanted_subtitle_languages,
                    keep_subtitle_languages: split_strings(&request.keep_subtitle_languages),
                    remove_track_ids,
                    preserve_chapters: request.preserve_chapters,
                    preserve_attachments: request.preserve_attachments,
                    preserve_source: request.preserve_original,
                    output_suffix: request.remux_output_suffix.clone(),
                    delete_source_after_success: request.delete_mp4_after_convert,
                    delete_external_subtitles_after_success: !request
                        .preserve_external_subtitle_files,
                },
                external_subtitles,
                extractions,
                existing_paths,
                authorized_roots: self.authorized_root_paths(),
                settings_fingerprint,
                tool_fingerprints: self.tool_fingerprints().await?,
                expires_in_seconds: 900,
                idempotency_key: key,
            })
            .map_err(RuntimeError::from)?;
        let mut context = base.context;
        let mut payload = base.payload;
        if let Some(template) = reorder_template.as_ref() {
            let desired_template_tracks =
                ordered_template_tracks(template, &request.reorder_template_track_ids)?;
            for item in &mut payload.items {
                let Some(file) = files
                    .iter()
                    .find(|file| same_path(&file.path, &item.source))
                else {
                    continue;
                };
                match track_order_for(file, &desired_template_tracks) {
                    Ok(order)
                        if order
                            != file
                                .tracks
                                .iter()
                                .map(|track| track.mkvmerge_id)
                                .collect::<Vec<_>>() =>
                    {
                        item.track_order_ids = order;
                        item.conflicts.retain(|conflict| {
                            conflict.kind != mkvo_domain::PlanConflictKind::NoChange
                        });
                    }
                    Ok(order) => {
                        item.track_order_ids = order;
                    }
                    Err(message) => item.conflicts.push(mkvo_domain::PlanConflict::blocking(
                        mkvo_domain::PlanConflictKind::InvalidSelection,
                        message,
                        Some(item.source.clone()),
                    )),
                }
            }
        }
        if request.format_subtitles {
            if request.subtitle_font_family.trim().is_empty() {
                return Err(RuntimeError::invalid("A subtitle font family is required."));
            }
            let font_size = request
                .subtitle_font_size
                .trim()
                .parse::<f32>()
                .map_err(|_| RuntimeError::invalid("Subtitle font size must be a number."))?;
            if !(1.0..=500.0).contains(&font_size) {
                return Err(RuntimeError::invalid(
                    "Subtitle font size must be between 1 and 500.",
                ));
            }
            validate_css_color(&request.subtitle_primary_color)?;
            validate_css_color(&request.subtitle_outline_color)?;
            let outline_size = request
                .subtitle_outline_size
                .trim()
                .parse::<f32>()
                .map_err(|_| RuntimeError::invalid("Subtitle outline size must be a number."))?;
            if !(0.0..=20.0).contains(&outline_size) {
                return Err(RuntimeError::invalid(
                    "Subtitle outline size must be between 0 and 20.",
                ));
            }
            validate_imported_font(
                &request.subtitle_font_file_name,
                &request.subtitle_font_data_base64,
            )?;
            for item in &mut payload.items {
                let Some(file) = files
                    .iter()
                    .find(|file| same_path(&file.path, &item.source))
                else {
                    continue;
                };
                let selected_track_ids = request
                    .subtitle_track_selections
                    .iter()
                    .find(|selection| same_path(Path::new(&selection.file_path), &item.source))
                    .map(|selection| selection.track_ids.iter().copied().collect::<BTreeSet<_>>());
                let transforms =
                    file.tracks
                        .iter()
                        .filter(|track| {
                            track.kind == TrackKind::Subtitle
                                && is_formatable_text_subtitle(track)
                                && selected_track_ids
                                    .as_ref()
                                    .is_none_or(|ids| ids.contains(&track.mkvmerge_id))
                        })
                        .map(|track| {
                            let stem = item.source.file_stem().map_or_else(String::new, |value| {
                                value.to_string_lossy().into_owned()
                            });
                            mkvo_domain::SubtitleTransform {
                                track_id: track.mkvmerge_id,
                                temporary_path: item.source.with_file_name(format!(
                                    ".{stem}.mkvo-subtitle-{}.ass",
                                    track.mkvmerge_id
                                )),
                                source_format: if is_srt_track(track) { "srt" } else { "ass" }
                                    .to_owned(),
                                language: track.language_or_undetermined().to_owned(),
                                name: track.name.clone(),
                                default: track.default,
                                forced: track.forced,
                            }
                        })
                        .collect::<Vec<_>>();
                if transforms.is_empty() {
                    item.conflicts.push(mkvo_domain::PlanConflict::blocking(
                        mkvo_domain::PlanConflictKind::InvalidSelection,
                        "No compatible ASS/SSA or SRT subtitle tracks were selected. Bitmap subtitles such as PGS cannot have their font changed.",
                        Some(item.source.clone()),
                    ));
                    continue;
                }
                let transformed_ids = transforms
                    .iter()
                    .map(|track| track.track_id)
                    .collect::<BTreeSet<_>>();
                item.selected_track_ids
                    .retain(|id| !transformed_ids.contains(id));
                item.external_subtitles
                    .extend(transforms.iter().map(|track| ExternalSubtitle {
                        path: track.temporary_path.clone(),
                        language: track.language.clone(),
                        name: track.name.clone(),
                        default: track.default,
                        forced: track.forced,
                    }));
                item.mux_track_order = file
                    .tracks
                    .iter()
                    .map(|track| {
                        transforms
                            .iter()
                            .position(|candidate| candidate.track_id == track.mkvmerge_id)
                            .map_or_else(
                                || format!("0:{}", track.mkvmerge_id),
                                |index| format!("{}:0", index + 1),
                            )
                    })
                    .collect();
                item.subtitle_transforms = transforms;
                item.conflicts
                    .retain(|conflict| conflict.kind != mkvo_domain::PlanConflictKind::NoChange);
            }
        }
        for item in &mut payload.items {
            if item.mode == RemuxMode::ExtractSubtitles {
                continue;
            }
            if !same_path(&item.source, &item.final_output)
                && self
                    .dependencies()
                    .file_system
                    .exists(&item.final_output)
                    .await?
                && !item
                    .conflicts
                    .iter()
                    .any(|conflict| conflict.kind == mkvo_domain::PlanConflictKind::ExistingTarget)
            {
                item.conflicts.push(mkvo_domain::PlanConflict::blocking(
                    mkvo_domain::PlanConflictKind::ExistingTarget,
                    "Remux output already exists",
                    Some(item.final_output.clone()),
                ));
            }
        }
        context.attributes.insert(
            "preserveChapters".to_owned(),
            request.preserve_chapters.to_string(),
        );
        context.attributes.insert(
            "preserveAttachments".to_owned(),
            request.preserve_attachments.to_string(),
        );
        context.attributes.insert(
            "preserveOriginal".to_owned(),
            request.preserve_original.to_string(),
        );
        context.attributes.insert(
            "extractOverwriteExistingFiles".to_owned(),
            request.extract_overwrite_existing_files.to_string(),
        );
        context.attributes.insert(
            "skipMuxIfSubtitleAlreadyExists".to_owned(),
            request.skip_mux_if_subtitle_already_exists.to_string(),
        );
        context.attributes.insert(
            "externalSubtitleTrackName".to_owned(),
            request
                .external_subtitle_track_name
                .clone()
                .unwrap_or_default(),
        );
        context.attributes.insert(
            "subtitleFontFamily".to_owned(),
            request.subtitle_font_family.trim().to_owned(),
        );
        context.attributes.insert(
            "subtitleFontSize".to_owned(),
            request.subtitle_font_size.trim().to_owned(),
        );
        context.attributes.insert(
            "subtitlePrimaryColor".to_owned(),
            request.subtitle_primary_color.trim().to_owned(),
        );
        context.attributes.insert(
            "subtitleOutlineColor".to_owned(),
            request.subtitle_outline_color.trim().to_owned(),
        );
        context.attributes.insert(
            "subtitleOutlineSize".to_owned(),
            request.subtitle_outline_size.trim().to_owned(),
        );
        context.attributes.insert(
            "subtitleFontFileName".to_owned(),
            request.subtitle_font_file_name.trim().to_owned(),
        );
        context.attributes.insert(
            "subtitleFontDataBase64".to_owned(),
            request.subtitle_font_data_base64.clone(),
        );
        context.attributes.insert(
            "subtitleTimingOffsetMs".to_owned(),
            request.subtitle_timing_offset_ms.to_string(),
        );
        let now = Utc::now();
        OperationPlan::new(
            base.metadata.kind,
            request,
            payload,
            context,
            now,
            now + Duration::minutes(15),
            base.metadata.idempotency_key,
        )
        .map_err(|error| RuntimeError::internal(error.to_string()))
    }

    pub(super) async fn start_mux_apply_impl(
        &self,
        request: MuxPreviewRequest,
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
        let plan: RemuxPlan = self.load_valid_plan(plan_id, &fingerprint, &key).await?;
        let total = u64::try_from(plan.payload.runnable_count()).unwrap_or(u64::MAX);
        let resources = plan.context.resources.clone();
        let runtime = self.clone();
        let fingerprint_for_task = fingerprint.clone();
        let key_for_task = key.clone();
        let accepted = self
            .jobs()
            .start(
                JobSpec {
                    kind: JobKind::from(plan.metadata.kind),
                    idempotency_key: key,
                    request_fingerprint: plan.metadata.request_fingerprint.clone(),
                    plan_id: Some(plan_id),
                    total,
                    resources,
                },
                move |context| async move {
                    let plan: RemuxPlan = runtime
                        .load_valid_plan(plan_id, &fingerprint_for_task, &key_for_task)
                        .await
                        .map_err(runtime_application_error)?;
                    runtime.execute_remux_plan(&plan, &context).await
                },
            )
            .await?;
        let snapshot = self
            .jobs()
            .get(accepted.id)
            .await?
            .ok_or_else(|| RuntimeError::internal("new mux job was not persisted"))?;
        Ok(crate::compat::OperationJobResponse::from_snapshot(
            &snapshot,
        ))
    }

    pub(super) async fn execute_remux_plan(
        &self,
        plan: &RemuxPlan,
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
                    .map(|item| item.source.to_string_lossy().into_owned()),
            )
            .await?;
        let total = u64::try_from(runnable.len()).unwrap_or(u64::MAX);
        let workers = self
            .settings_service()
            .load()
            .await?
            .settings
            .workers
            .max_remux_workers;
        let mut executions = stream::iter(runnable.into_iter().enumerate())
            .map(|(index, item)| async move {
                let key = item.source.to_string_lossy().into_owned();
                let result = self
                    .execute_remux_item(&item, &plan.context.attributes, context, index, total)
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
                    // A canceled file was not changed: it stays pending rather than failed.
                    if !matches!(error, ApplicationError::Canceled) {
                        journal.fail_item(&key, error.to_string());
                        context.record_failed().await?;
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
        journal.detail = Some("mux/remux plan completed".to_owned());
        journal.updated_utc = Utc::now();
        self.dependencies().journal.advance(&journal).await?;
        let mut response = mux_preview_response(plan);
        response.status = "Mux/remux operation completed".to_owned();
        self.append_log(
            "Mux",
            "Mux/remux plan completed",
            &plan.metadata.id.to_string(),
        )
        .await
        .map_err(runtime_application_error)?;
        Ok(JobCompletion {
            result: Some(serde_json::to_value(response).map_err(|error| {
                ApplicationError::Internal(format!("mux result serialization failed: {error}"))
            })?),
            message: Some("Mux/remux operation completed".to_owned()),
        })
    }

    pub(super) async fn execute_remux_item(
        &self,
        item: &mkvo_domain::RemuxPlanItem,
        attributes: &BTreeMap<String, String>,
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
                item.source.to_string_lossy(),
                0,
            )
            .await?;
        let outputs = self.remux_outputs(item).await;
        let produced = self.produce_remux_output(item, attributes, context).await;
        // Temporaries never outlive the item; one that cannot be removed is only a warning.
        let (kept, warnings) = self
            .remove_remux_temporaries(item, attributes, outputs)
            .await;
        for warning in warnings {
            let _ = context.log(JobLogLevel::Warning, warning).await;
        }
        match produced {
            Err(error) if !kept.is_empty() => {
                return Err(ApplicationError::Internal(format!(
                    "{error}; the file being replaced is gone, so the new file was kept at {}",
                    kept.join(", ")
                )));
            }
            produced => produced?,
        }
        if item.delete_external_subtitles_after_success {
            for subtitle in &item.external_subtitles {
                self.dependencies()
                    .file_system
                    .remove_file(&subtitle.path)
                    .await?;
            }
        }
        if item.mode != RemuxMode::ExtractSubtitles {
            if item.delete_source_after_success && !same_path(&item.source, &item.final_output) {
                self.apply_renames_to_working_set(&[(
                    item.source.clone(),
                    item.final_output.clone(),
                )])
                .await;
            } else {
                self.refresh_working_set(std::slice::from_ref(&item.final_output))
                    .await;
            }
        }
        Ok(())
    }

    async fn produce_remux_output(
        &self,
        item: &mkvo_domain::RemuxPlanItem,
        attributes: &BTreeMap<String, String>,
        context: &mkvo_application::JobContext,
    ) -> Result<(), ApplicationError> {
        let replaced_font_names = if item.subtitle_transforms.is_empty() {
            BTreeSet::new()
        } else {
            self.prepare_subtitle_transforms(item, attributes, context)
                .await?
        };
        let invocation = self
            .remux_invocation(item, attributes, &replaced_font_names)
            .await?;
        context
            .log(
                JobLogLevel::Information,
                format!("{} {}", remux_mode_label(item.mode), item.source.display()),
            )
            .await?;
        let execution = self
            .dependencies()
            .tool_executor
            .execute(&invocation, context.cancellation_token())
            .await?;
        if let Some(warning) = tool_warning(&invocation.tool, &execution)? {
            context
                .log(
                    JobLogLevel::Warning,
                    format!("{}: {warning}", item.source.display()),
                )
                .await?;
        }
        for expected in &invocation.expected_outputs {
            if !execution
                .validated_outputs
                .iter()
                .any(|path| same_path(path, expected))
            {
                return Err(ApplicationError::Internal(format!(
                    "{} did not create expected output {}",
                    invocation.tool,
                    expected.display()
                )));
            }
        }
        if item.mode == RemuxMode::ExtractSubtitles {
            self.promote_extractions(item, attributes).await
        } else {
            self.promote_remux_output(item).await
        }
    }

    /// Each temporary the tool writes, with the existing file it is about to replace.
    async fn remux_outputs(
        &self,
        item: &mkvo_domain::RemuxPlanItem,
    ) -> Vec<(PathBuf, Option<PathBuf>)> {
        if item.mode != RemuxMode::ExtractSubtitles {
            let replaced =
                same_path(&item.source, &item.final_output).then(|| item.final_output.clone());
            return vec![(item.temporary_output.clone(), replaced)];
        }
        let file_system = &self.dependencies().file_system;
        let mut outputs = Vec::new();
        for track in &item.extract_tracks {
            let exists = file_system.exists(&track.output).await.unwrap_or(false);
            outputs.push((
                extraction_temp_path(&track.output, track.track_id),
                exists.then(|| track.output.clone()),
            ));
        }
        outputs
    }

    /// Removes what the item left behind; returns the outputs kept and what could not be removed.
    async fn remove_remux_temporaries(
        &self,
        item: &mkvo_domain::RemuxPlanItem,
        attributes: &BTreeMap<String, String>,
        outputs: Vec<(PathBuf, Option<PathBuf>)>,
    ) -> (Vec<String>, Vec<String>) {
        let mut temporaries: Vec<(PathBuf, Option<PathBuf>)> = item
            .subtitle_transforms
            .iter()
            .map(|transform| (transform.temporary_path.clone(), None))
            .collect();
        temporaries.extend(imported_font_path(item, attributes).map(|font| (font, None)));
        temporaries.extend(outputs);
        let file_system = &self.dependencies().file_system;
        let mut kept = Vec::new();
        let mut failures = Vec::new();
        for (temporary, replaced) in temporaries {
            if !file_system.exists(&temporary).await.unwrap_or(false) {
                continue;
            }
            // A failed replace may already have taken the old file: then this is the only copy.
            if let Some(replaced) = replaced {
                if !file_system.exists(&replaced).await.unwrap_or(false) {
                    kept.push(temporary.display().to_string());
                    continue;
                }
            }
            if let Err(error) = file_system.remove_file(&temporary).await {
                failures.push(format!("temporary file was left behind: {error}"));
            }
        }
        (kept, failures)
    }

    async fn prepare_subtitle_transforms(
        &self,
        item: &mkvo_domain::RemuxPlanItem,
        attributes: &BTreeMap<String, String>,
        context: &mkvo_application::JobContext,
    ) -> Result<BTreeSet<String>, ApplicationError> {
        let executable = self.tool_path("mkvextract").await?;
        for track in &item.subtitle_transforms {
            if self
                .dependencies()
                .file_system
                .exists(&track.temporary_path)
                .await?
            {
                self.dependencies()
                    .file_system
                    .remove_file(&track.temporary_path)
                    .await?;
            }
        }
        let font_name = attributes
            .get("subtitleFontFileName")
            .map_or("", String::as_str);
        let font_data = attributes
            .get("subtitleFontDataBase64")
            .map_or("", String::as_str);
        if !font_name.is_empty() && !font_data.is_empty() {
            let path = imported_font_path(item, attributes).ok_or_else(|| {
                ApplicationError::InvalidRequest("Invalid imported font file name.".to_owned())
            })?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(font_data)
                .map_err(|_| {
                    ApplicationError::InvalidRequest(
                        "Imported font data is not valid base64.".to_owned(),
                    )
                })?;
            tokio::fs::write(&path, bytes).await.map_err(|error| {
                ApplicationError::Internal(format!("could not stage imported font: {error}"))
            })?;
        }
        let mut arguments = vec![
            "tracks".to_owned(),
            item.source.to_string_lossy().into_owned(),
        ];
        arguments.extend(item.subtitle_transforms.iter().map(|track| {
            format!(
                "{}:{}",
                track.track_id,
                track.temporary_path.to_string_lossy()
            )
        }));
        let invocation = ToolInvocation {
            tool: "mkvextract".to_owned(),
            executable,
            arguments,
            working_directory: item.source.parent().map(Path::to_path_buf),
            expected_outputs: item
                .subtitle_transforms
                .iter()
                .map(|track| track.temporary_path.clone())
                .collect(),
        };
        let execution = self
            .dependencies()
            .tool_executor
            .execute(&invocation, context.cancellation_token())
            .await?;
        if let Some(warning) = tool_warning(&invocation.tool, &execution)? {
            context
                .log(
                    JobLogLevel::Warning,
                    format!("{}: {warning}", item.source.display()),
                )
                .await?;
        }
        let font = attributes
            .get("subtitleFontFamily")
            .map_or("", String::as_str);
        let font_size = attributes
            .get("subtitleFontSize")
            .map_or("48", String::as_str);
        let primary_color = css_to_ass_color(
            attributes
                .get("subtitlePrimaryColor")
                .map_or("#FFFFFF", String::as_str),
        )?;
        let outline_color = css_to_ass_color(
            attributes
                .get("subtitleOutlineColor")
                .map_or("#000000", String::as_str),
        )?;
        let outline_size = attributes
            .get("subtitleOutlineSize")
            .map_or("2", String::as_str);
        let offset = attributes
            .get("subtitleTimingOffsetMs")
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let mut replaced_font_names = BTreeSet::new();
        for track in &item.subtitle_transforms {
            let content = tokio::fs::read_to_string(&track.temporary_path)
                .await
                .map_err(|error| {
                    ApplicationError::Internal(format!(
                        "could not read extracted subtitle {}: {error}",
                        track.temporary_path.display()
                    ))
                })?;
            let transformed = if track.source_format.eq_ignore_ascii_case("srt") {
                transform_srt_document(
                    &content,
                    font,
                    font_size,
                    &primary_color,
                    &outline_color,
                    outline_size,
                    offset,
                )?
            } else {
                replaced_font_names.extend(ass_font_names(&content));
                transform_ass_document(
                    &content,
                    font,
                    font_size,
                    &primary_color,
                    &outline_color,
                    outline_size,
                    offset,
                )?
            };
            tokio::fs::write(&track.temporary_path, transformed)
                .await
                .map_err(|error| {
                    ApplicationError::Internal(format!(
                        "could not write transformed subtitle {}: {error}",
                        track.temporary_path.display()
                    ))
                })?;
        }
        Ok(replaced_font_names)
    }

    pub(super) async fn remux_invocation(
        &self,
        item: &mkvo_domain::RemuxPlanItem,
        attributes: &BTreeMap<String, String>,
        replaced_font_names: &BTreeSet<String>,
    ) -> Result<ToolInvocation, ApplicationError> {
        let tool = remux_tool_name(item.mode);
        let executable = self.tool_path(tool).await?;
        if item.mode == RemuxMode::ExtractSubtitles {
            let mut arguments = vec![
                "tracks".to_owned(),
                item.source.to_string_lossy().into_owned(),
            ];
            arguments.extend(item.extract_tracks.iter().map(|track| {
                format!(
                    "{}:{}",
                    track.track_id,
                    extraction_temp_path(&track.output, track.track_id).to_string_lossy()
                )
            }));
            return Ok(ToolInvocation {
                tool: tool.to_owned(),
                executable,
                arguments,
                working_directory: item.source.parent().map(Path::to_path_buf),
                expected_outputs: item
                    .extract_tracks
                    .iter()
                    .map(|track| extraction_temp_path(&track.output, track.track_id))
                    .collect(),
            });
        }
        let mut arguments = vec![
            "-o".to_owned(),
            item.temporary_output.to_string_lossy().into_owned(),
        ];
        if attributes
            .get("preserveChapters")
            .is_some_and(|value| value == "false")
        {
            arguments.push("--no-chapters".to_owned());
        }
        let preserve_attachments = attributes
            .get("preserveAttachments")
            .is_none_or(|value| value != "false");
        if !preserve_attachments {
            arguments.push("--no-attachments".to_owned());
        }
        let source = self
            .current_domain_files()
            .await
            .into_iter()
            .find(|file| same_path(&file.path, &item.source));
        if let Some(source) = source.as_ref() {
            append_track_selection(&mut arguments, source, &item.selected_track_ids);
            if preserve_attachments && !item.subtitle_transforms.is_empty() {
                let all_formatable_tracks_selected = source
                    .tracks
                    .iter()
                    .filter(|track| {
                        track.kind == TrackKind::Subtitle && is_formatable_text_subtitle(track)
                    })
                    .all(|track| {
                        item.subtitle_transforms
                            .iter()
                            .any(|transform| transform.track_id == track.mkvmerge_id)
                    });
                append_subtitle_font_attachment_selection(
                    &mut arguments,
                    &source.attachments,
                    attributes
                        .get("subtitleFontFileName")
                        .map_or("", String::as_str),
                    all_formatable_tracks_selected.then_some(replaced_font_names),
                );
            }
        }
        if !item.mux_track_order.is_empty() {
            arguments.push("--track-order".to_owned());
            arguments.push(item.mux_track_order.join(","));
        } else if !item.track_order_ids.is_empty() {
            arguments.push("--track-order".to_owned());
            arguments.push(
                item.track_order_ids
                    .iter()
                    .map(|id| format!("0:{id}"))
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        arguments.push(item.source.to_string_lossy().into_owned());
        for subtitle in &item.external_subtitles {
            if !subtitle.language.trim().is_empty() {
                arguments.push("--language".to_owned());
                arguments.push(format!("0:{}", subtitle.language.trim()));
            }
            if subtitle.forced {
                arguments.push("--forced-display-flag".to_owned());
                arguments.push("0:yes".to_owned());
            }
            arguments.push("--default-track-flag".to_owned());
            arguments.push(format!("0:{}", if subtitle.default { "yes" } else { "no" }));
            if let Some(name) = subtitle
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
            {
                arguments.push("--track-name".to_owned());
                arguments.push(format!("0:{}", name.trim()));
            }
            arguments.push(subtitle.path.to_string_lossy().into_owned());
        }
        if let Some(font_path) = imported_font_path(item, attributes) {
            let extension = font_path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("ttf");
            arguments.push("--attachment-name".to_owned());
            arguments.push(
                attributes
                    .get("subtitleFontFileName")
                    .cloned()
                    .unwrap_or_default(),
            );
            arguments.push("--attachment-mime-type".to_owned());
            arguments.push(
                if extension.eq_ignore_ascii_case("otf") {
                    "application/vnd.ms-opentype"
                } else {
                    "application/x-truetype-font"
                }
                .to_owned(),
            );
            arguments.push("--attach-file".to_owned());
            arguments.push(font_path.to_string_lossy().into_owned());
        }
        Ok(ToolInvocation {
            tool: tool.to_owned(),
            executable,
            arguments,
            working_directory: item.source.parent().map(Path::to_path_buf),
            expected_outputs: vec![item.temporary_output.clone()],
        })
    }

    pub(super) async fn promote_remux_output(
        &self,
        item: &mkvo_domain::RemuxPlanItem,
    ) -> Result<(), ApplicationError> {
        if same_path(&item.source, &item.final_output) {
            // One rename: there is no moment without a complete file at this path.
            self.dependencies()
                .file_system
                .replace_file(&item.temporary_output, &item.final_output)
                .await?;
        } else {
            self.dependencies()
                .file_system
                .move_file(&item.temporary_output, &item.final_output)
                .await?;
            if item.delete_source_after_success {
                self.dependencies()
                    .file_system
                    .remove_file(&item.source)
                    .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn promote_extractions(
        &self,
        item: &mkvo_domain::RemuxPlanItem,
        attributes: &BTreeMap<String, String>,
    ) -> Result<(), ApplicationError> {
        let overwrite = attributes
            .get("extractOverwriteExistingFiles")
            .is_some_and(|value| value == "true");
        for track in &item.extract_tracks {
            let temporary = extraction_temp_path(&track.output, track.track_id);
            if self
                .dependencies()
                .file_system
                .exists(&track.output)
                .await?
            {
                if !overwrite {
                    return Err(ApplicationError::Conflict(format!(
                        "subtitle output already exists: {}",
                        track.output.display()
                    )));
                }
                self.dependencies()
                    .file_system
                    .replace_file(&temporary, &track.output)
                    .await?;
            } else {
                self.dependencies()
                    .file_system
                    .move_file(&temporary, &track.output)
                    .await?;
            }
        }
        Ok(())
    }
}

/// One error for a whole batch, naming the files that failed rather than only the first.
pub(super) fn batch_failure(
    failures: Vec<(String, ApplicationError)>,
    total: u64,
) -> ApplicationError {
    const LISTED: usize = 5;
    let mut failed: Vec<_> = failures
        .into_iter()
        .filter(|(_, error)| !matches!(error, ApplicationError::Canceled))
        .collect();
    if failed.is_empty() {
        return ApplicationError::Canceled;
    }
    if total == 1 {
        return failed.remove(0).1;
    }
    let mut message = format!("{} of {total} files failed: ", failed.len());
    message += &failed
        .iter()
        .take(LISTED)
        .map(|(key, error)| {
            let reason = match error {
                ApplicationError::Internal(reason) => reason.clone(),
                error => error.to_string(),
            };
            format!("{}: {reason}", file_name(Path::new(key)))
        })
        .collect::<Vec<_>>()
        .join("; ");
    if failed.len() > LISTED {
        message += &format!(
            "; and {} more, listed in the job log",
            failed.len() - LISTED
        );
    }
    ApplicationError::Internal(message)
}

/// MKVToolNix exits with 1 when it finished with warnings and with 2 when it failed.
pub(super) fn tool_warning(
    tool: &str,
    execution: &mkvo_application::ToolExecutionResult,
) -> Result<Option<String>, ApplicationError> {
    match execution.exit_code {
        Some(0) => Ok(None),
        Some(1) if finished(tool, &execution.stdout) => Ok(Some(format!(
            "{tool} finished with warnings: {}",
            tool_message(execution, "Warning:")
        ))),
        code => Err(ApplicationError::Internal(format!(
            "{tool} {} ({}): {}",
            if code == Some(1) {
                "did not finish"
            } else {
                "failed"
            },
            code.map_or_else(
                || "no exit code".to_owned(),
                |code| format!("exit code {code}")
            ),
            tool_message(execution, "Error:")
        ))),
    }
}

// A tool killed on Windows also exits with 1; only its last line (English is forced) proves it ended.
fn finished(tool: &str, stdout: &str) -> bool {
    let last = stdout
        .split(['\r', '\n'])
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or_default();
    match tool {
        "mkvmerge" => last.starts_with("Multiplexing took"),
        "mkvpropedit" => last == "Done." || last == "No changes were made.",
        // mkvextract prints nothing after its last progress line.
        _ => false,
    }
}

// MKVToolNix prints its messages to stdout, among the progress lines.
fn tool_message(execution: &mkvo_application::ToolExecutionResult, prefix: &str) -> String {
    let output = if execution.stderr.trim().is_empty() {
        &execution.stdout
    } else {
        &execution.stderr
    };
    let lines: Vec<&str> = output
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let flagged: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| line.starts_with(prefix))
        .collect();
    let shown = if flagged.is_empty() { lines } else { flagged };
    shown[shown.len().saturating_sub(5)..].join(" | ")
}

fn ordered_template_tracks<'a>(
    template: &'a MediaFile,
    requested_ids: &[u64],
) -> RuntimeResult<Vec<&'a MediaTrack>> {
    let ids = if requested_ids.is_empty() {
        template
            .tracks
            .iter()
            .map(|track| track.mkvmerge_id)
            .collect::<Vec<_>>()
    } else {
        requested_ids.to_vec()
    };
    if ids.len() != template.tracks.len() {
        return Err(RuntimeError::invalid(
            "Template order must contain every track exactly once.",
        ));
    }
    let mut ordered = Vec::with_capacity(ids.len());
    for id in ids {
        if ordered
            .iter()
            .any(|track: &&MediaTrack| track.mkvmerge_id == id)
        {
            return Err(RuntimeError::invalid(
                "Template order contains a duplicate track.",
            ));
        }
        ordered.push(
            template
                .tracks
                .iter()
                .find(|track| track.mkvmerge_id == id)
                .ok_or_else(|| {
                    RuntimeError::invalid(format!("Template track ID {id} was not found."))
                })?,
        );
    }
    Ok(ordered)
}

fn track_order_for(file: &MediaFile, template_tracks: &[&MediaTrack]) -> Result<Vec<u64>, String> {
    if file.tracks.len() != template_tracks.len() {
        return Err(format!(
            "Track count does not match template ({} vs {}).",
            file.tracks.len(),
            template_tracks.len()
        ));
    }
    let mut used = BTreeSet::new();
    let mut order = Vec::with_capacity(template_tracks.len());
    for template in template_tracks {
        let matches = file
            .tracks
            .iter()
            .filter(|track| !used.contains(&track.mkvmerge_id) && tracks_match(template, track))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(format!(
                "Could not uniquely match template track {} ({} / {} / {}).",
                template.propedit_track_number,
                track_kind_label(template.kind),
                template.language_or_undetermined(),
                template.name.as_deref().unwrap_or("unnamed")
            ));
        }
        used.insert(matches[0].mkvmerge_id);
        order.push(matches[0].mkvmerge_id);
    }
    Ok(order)
}

fn tracks_match(left: &MediaTrack, right: &MediaTrack) -> bool {
    left.kind == right.kind
        && left
            .language_or_undetermined()
            .eq_ignore_ascii_case(right.language_or_undetermined())
        && left
            .name
            .as_deref()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case(right.name.as_deref().unwrap_or("").trim())
        && left
            .codec_id
            .as_deref()
            .unwrap_or(&left.codec)
            .eq_ignore_ascii_case(right.codec_id.as_deref().unwrap_or(&right.codec))
        && left.channels == right.channels
}

fn subtitle_codec_matches(codec: &str, codec_id: Option<&str>, subtitle_path: &Path) -> bool {
    let extension = subtitle_path.extension().map_or_else(String::new, |value| {
        value.to_string_lossy().to_ascii_lowercase()
    });
    let codec = codec.to_ascii_lowercase();
    let codec_id = codec_id.unwrap_or_default().to_ascii_lowercase();
    match extension.as_str() {
        "srt" => codec.contains("subrip") || codec.contains("srt") || codec_id == "s_text/utf8",
        "ass" => codec.contains("ass") || codec_id == "s_text/ass",
        "ssa" => codec.contains("ssa") || codec_id == "s_text/ssa",
        "sub" | "idx" => codec.contains("vobsub") || codec_id == "s_vobsub",
        "sup" => codec.contains("pgs") || codec.contains("hdmv") || codec_id == "s_hdmv/pgs",
        _ => false,
    }
}

fn subtitle_names_match(existing: Option<&str>, candidate: Option<&str>) -> bool {
    let existing = existing.map(str::trim).filter(|value| !value.is_empty());
    let candidate = candidate.map(str::trim).filter(|value| !value.is_empty());
    match (existing, candidate) {
        (None, None) => true,
        (Some(existing), Some(candidate)) => existing.eq_ignore_ascii_case(candidate),
        _ => false,
    }
}

fn is_editable_ass_track(track: &MediaTrack) -> bool {
    let codec = track.codec.to_ascii_lowercase();
    let codec_id = track
        .codec_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    codec.contains("substation")
        || codec == "ass"
        || codec == "ssa"
        || codec_id == "s_text/ass"
        || codec_id == "s_text/ssa"
}

fn is_srt_track(track: &MediaTrack) -> bool {
    let codec = track.codec.to_ascii_lowercase();
    let codec_id = track
        .codec_id
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    codec.contains("subrip") || codec == "srt" || codec_id == "s_text/utf8"
}

fn is_formatable_text_subtitle(track: &MediaTrack) -> bool {
    is_editable_ass_track(track) || is_srt_track(track)
}

fn append_subtitle_font_attachment_selection(
    arguments: &mut Vec<String>,
    attachments: &[mkvo_domain::MediaAttachment],
    selected_font_file_name: &str,
    replaced_font_names: Option<&BTreeSet<String>>,
) {
    let retained = attachments
        .iter()
        .filter(|attachment| {
            if !is_font_attachment(attachment) {
                return true;
            }
            if !selected_font_file_name.is_empty()
                && attachment
                    .file_name
                    .eq_ignore_ascii_case(selected_font_file_name)
            {
                return false;
            }
            // When every compatible text track is being formatted, all style and inline font
            // references are rewritten to the selected font. No inherited font attachment remains
            // referenced, even when its file name differs from its internal family name.
            replaced_font_names.is_none()
        })
        .map(|attachment| attachment.id)
        .collect::<Vec<_>>();
    if retained.is_empty() {
        arguments.push("--no-attachments".to_owned());
    } else if retained.len() != attachments.len() {
        arguments.push("--attachments".to_owned());
        arguments.push(
            retained
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
    }
}

fn font_match_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn ass_font_names(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut section = "";
    let mut font_index = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed;
        }
        if section.eq_ignore_ascii_case("[V4+ Styles]")
            || section.eq_ignore_ascii_case("[V4 Styles]")
        {
            if let Some(value) = trimmed.strip_prefix("Format:") {
                font_index = value
                    .split(',')
                    .map(str::trim)
                    .position(|field| field.eq_ignore_ascii_case("Fontname"));
            } else if let Some(value) = line.strip_prefix("Style:") {
                if let Some(name) = font_index
                    .and_then(|index| value.split(',').nth(index))
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    names.insert(font_match_key(name));
                }
            }
        }
        let mut rest = line;
        while let Some(index) = rest.find("\\fn") {
            let value = &rest[index + 3..];
            let end = value.find(['\\', '}']).unwrap_or(value.len());
            let name = value[..end].trim();
            if !name.is_empty() {
                names.insert(font_match_key(name));
            }
            rest = &value[end..];
        }
    }
    names
}

fn is_font_attachment(attachment: &mkvo_domain::MediaAttachment) -> bool {
    let extension = Path::new(&attachment.file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let content_type = attachment
        .content_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "ttf" | "otf" | "ttc" | "woff" | "woff2"
    ) || content_type.contains("font")
        || content_type.contains("truetype")
        || content_type.contains("opentype")
}

fn transform_srt_document(
    content: &str,
    font: &str,
    font_size: &str,
    primary_color: &str,
    outline_color: &str,
    outline_size: &str,
    offset_ms: i64,
) -> Result<String, ApplicationError> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut dialogues = Vec::new();
    for block in normalized.split("\n\n") {
        let lines = block.lines().collect::<Vec<_>>();
        let Some(timing_index) = lines.iter().position(|line| line.contains(" --> ")) else {
            continue;
        };
        let timing = lines[timing_index].split_once(" --> ").ok_or_else(|| {
            ApplicationError::InvalidRequest("Invalid SRT cue timing.".to_owned())
        })?;
        let start = srt_time_to_ass(timing.0, offset_ms)?;
        let end_value = timing.1.split_whitespace().next().unwrap_or(timing.1);
        let end = srt_time_to_ass(end_value, offset_ms)?;
        let text = lines
            .get(timing_index + 1..)
            .unwrap_or_default()
            .iter()
            .map(|line| srt_text_to_ass(line))
            .collect::<Vec<_>>()
            .join("\\N");
        dialogues.push(format!("Dialogue: 0,{start},{end},Default,,0,0,0,,{text}"));
    }
    if dialogues.is_empty() {
        return Err(ApplicationError::InvalidRequest(
            "The selected SRT track contains no valid cues.".to_owned(),
        ));
    }
    Ok(format!(
        "[Script Info]\nScriptType: v4.00+\nWrapStyle: 0\nScaledBorderAndShadow: yes\nYCbCr Matrix: TV.709\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,{font},{font_size},{primary_color},{primary_color},{outline_color},&H00000000,0,0,0,0,100,100,0,0,1,{outline_size},0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n{}\n",
        dialogues.join("\n")
    ))
}

fn srt_time_to_ass(value: &str, offset_ms: i64) -> Result<String, ApplicationError> {
    let parts = value
        .trim()
        .replace(',', ".")
        .split(':')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(ApplicationError::InvalidRequest(format!(
            "Invalid SRT timestamp: {value}"
        )));
    }
    let hours = parts[0]
        .parse::<i64>()
        .map_err(|_| ApplicationError::InvalidRequest(format!("Invalid SRT timestamp: {value}")))?;
    let minutes = parts[1]
        .parse::<i64>()
        .map_err(|_| ApplicationError::InvalidRequest(format!("Invalid SRT timestamp: {value}")))?;
    let seconds = parts[2]
        .parse::<f64>()
        .map_err(|_| ApplicationError::InvalidRequest(format!("Invalid SRT timestamp: {value}")))?;
    let millis =
        ((hours * 3_600 + minutes * 60) * 1_000 + (seconds * 1_000.0).round() as i64 + offset_ms)
            .max(0);
    Ok(format!(
        "{}:{:02}:{:02}.{:02}",
        millis / 3_600_000,
        (millis / 60_000) % 60,
        (millis / 1_000) % 60,
        (millis % 1_000) / 10
    ))
}

fn srt_text_to_ass(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace("<i>", "{\\i1}")
        .replace("</i>", "{\\i0}")
        .replace("<b>", "{\\b1}")
        .replace("</b>", "{\\b0}")
        .replace("<u>", "{\\u1}")
        .replace("</u>", "{\\u0}")
}

fn transform_ass_document(
    content: &str,
    font: &str,
    font_size: &str,
    primary_color: &str,
    outline_color: &str,
    outline_size: &str,
    offset_ms: i64,
) -> Result<String, ApplicationError> {
    let mut section = "";
    let mut style_font_index = None;
    let mut style_font_size_index = None;
    let mut style_primary_index = None;
    let mut style_outline_color_index = None;
    let mut style_outline_size_index = None;
    let mut event_start_index = None;
    let mut event_end_index = None;
    let mut event_text_index = None;
    let mut output = String::with_capacity(content.len());
    for raw in content.split_inclusive('\n') {
        let newline = if raw.ends_with("\r\n") {
            "\r\n"
        } else if raw.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let line = raw.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed;
        }
        let mut next = line.to_owned();
        if section.eq_ignore_ascii_case("[V4+ Styles]")
            || section.eq_ignore_ascii_case("[V4 Styles]")
        {
            if let Some(value) = trimmed.strip_prefix("Format:") {
                let fields = value.split(',').map(str::trim).collect::<Vec<_>>();
                style_font_index = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("Fontname"));
                style_font_size_index = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("Fontsize"));
                style_primary_index = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("PrimaryColour"));
                style_outline_color_index = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("OutlineColour"));
                style_outline_size_index = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("Outline"));
            } else if let Some(value) = line.strip_prefix("Style:") {
                let mut fields = value.split(',').map(str::to_owned).collect::<Vec<_>>();
                set_ass_style_field(&mut fields, style_font_index, font);
                set_ass_style_field(&mut fields, style_font_size_index, font_size);
                set_ass_style_field(&mut fields, style_primary_index, primary_color);
                set_ass_style_field(&mut fields, style_outline_color_index, outline_color);
                set_ass_style_field(&mut fields, style_outline_size_index, outline_size);
                next = format!("Style:{}", fields.join(","));
            }
        } else if section.eq_ignore_ascii_case("[Events]") {
            if let Some(value) = trimmed.strip_prefix("Format:") {
                let fields = value.split(',').map(str::trim).collect::<Vec<_>>();
                event_start_index = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("Start"));
                event_end_index = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("End"));
                event_text_index = fields
                    .iter()
                    .position(|field| field.eq_ignore_ascii_case("Text"));
            } else if let Some(value) = line.strip_prefix("Dialogue:") {
                let mut fields = value.split(',').map(str::to_owned).collect::<Vec<_>>();
                if let (Some(start), Some(end)) = (event_start_index, event_end_index) {
                    if offset_ms != 0 && start < fields.len() && end < fields.len() {
                        fields[start] = shift_ass_time(&fields[start], offset_ms)?;
                        fields[end] = shift_ass_time(&fields[end], offset_ms)?;
                    }
                }
                if let Some(text) = event_text_index.filter(|index| *index < fields.len()) {
                    fields[text] = replace_ass_inline_fonts(&fields[text..].join(","), font);
                    fields.truncate(text + 1);
                }
                next = format!("Dialogue:{}", fields.join(","));
            }
        }
        output.push_str(&next);
        output.push_str(newline);
    }
    Ok(output)
}

fn set_ass_style_field(fields: &mut [String], index: Option<usize>, value: &str) {
    if let Some(index) = index.filter(|index| *index < fields.len()) {
        fields[index] = value.to_owned();
    }
}

fn validate_css_color(value: &str) -> RuntimeResult<()> {
    let value = value.trim();
    if value.len() == 7
        && value.starts_with('#')
        && value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Ok(());
    }
    Err(RuntimeError::invalid(
        "Subtitle colors must use #RRGGBB format.",
    ))
}

fn css_to_ass_color(value: &str) -> Result<String, ApplicationError> {
    validate_css_color(value)
        .map_err(|error| ApplicationError::InvalidRequest(error.to_string()))?;
    let hex = &value.trim()[1..];
    Ok(format!("&H00{}{}{}", &hex[4..6], &hex[2..4], &hex[0..2]))
}

fn validate_imported_font(file_name: &str, data: &str) -> RuntimeResult<()> {
    if file_name.trim().is_empty() && data.is_empty() {
        return Ok(());
    }
    if file_name.trim().is_empty() || data.is_empty() {
        return Err(RuntimeError::invalid(
            "Imported font name and data must both be provided.",
        ));
    }
    if file_name.trim().starts_with('@') {
        return Err(RuntimeError::invalid(
            "Imported font name must not start with @.",
        ));
    }
    let path = Path::new(file_name.trim());
    if path.file_name().and_then(|value| value.to_str()) != Some(file_name.trim()) {
        return Err(RuntimeError::invalid(
            "Imported font name must not contain a path.",
        ));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case("ttf") && !extension.eq_ignore_ascii_case("otf") {
        return Err(RuntimeError::invalid(
            "Imported fonts must be .ttf or .otf files.",
        ));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| RuntimeError::invalid("Imported font data is not valid base64."))?;
    if decoded.len() > 1024 * 1024 {
        return Err(RuntimeError::invalid(
            "Imported fonts must be 1 MB or smaller.",
        ));
    }
    Ok(())
}

fn imported_font_path(
    item: &mkvo_domain::RemuxPlanItem,
    attributes: &BTreeMap<String, String>,
) -> Option<PathBuf> {
    let name = attributes.get("subtitleFontFileName")?.trim();
    // mkvmerge would load `--attachment-name @x` as a file of further options.
    if name.is_empty()
        || name.starts_with('@')
        || Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name)
    {
        return None;
    }
    let source_name = item.source.file_stem()?.to_string_lossy();
    Some(
        item.source
            .with_file_name(format!(".{source_name}.mkvo-font-{name}")),
    )
}

fn replace_ass_inline_fonts(text: &str, font: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find("\\fn") {
        output.push_str(&rest[..index + 3]);
        rest = &rest[index + 3..];
        let end = rest.find(['\\', '}']).unwrap_or(rest.len());
        output.push_str(font);
        rest = &rest[end..];
    }
    output.push_str(rest);
    output
}

fn shift_ass_time(value: &str, offset_ms: i64) -> Result<String, ApplicationError> {
    let parts = value.trim().split(':').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(ApplicationError::Internal(format!(
            "invalid ASS timestamp: {value}"
        )));
    }
    let hours = parts[0]
        .parse::<i64>()
        .map_err(|_| ApplicationError::Internal(format!("invalid ASS timestamp: {value}")))?;
    let minutes = parts[1]
        .parse::<i64>()
        .map_err(|_| ApplicationError::Internal(format!("invalid ASS timestamp: {value}")))?;
    let seconds = parts[2]
        .parse::<f64>()
        .map_err(|_| ApplicationError::Internal(format!("invalid ASS timestamp: {value}")))?;
    let millis =
        ((hours * 3600 + minutes * 60) * 1000 + (seconds * 1000.0).round() as i64 + offset_ms)
            .max(0);
    Ok(format!(
        "{}:{:02}:{:02}.{:02}",
        millis / 3_600_000,
        (millis / 60_000) % 60,
        (millis / 1000) % 60,
        (millis % 1000) / 10
    ))
}

#[cfg(test)]
mod subtitle_match_tests {
    use super::{
        append_subtitle_font_attachment_selection, ass_font_names, font_match_key, shift_ass_time,
        subtitle_codec_matches, subtitle_names_match, track_order_for, transform_ass_document,
        transform_srt_document, validate_imported_font,
    };
    use chrono::Utc;
    use mkvo_domain::{
        ContainerMetadata, FileFingerprint, MediaAttachment, MediaFile, MediaStatus, MediaTrack,
        TrackKind,
    };
    use std::collections::BTreeSet;
    use std::path::Path;

    fn track(id: u64, number: u32, kind: TrackKind, name: &str) -> MediaTrack {
        MediaTrack {
            mkvmerge_id: id,
            propedit_track_number: number,
            kind,
            codec: if kind == TrackKind::Subtitle {
                "SubStationAlpha"
            } else {
                "HEVC"
            }
            .to_owned(),
            codec_id: None,
            language: Some(
                if kind == TrackKind::Subtitle {
                    "eng"
                } else {
                    "und"
                }
                .to_owned(),
            ),
            name: (!name.is_empty()).then(|| name.to_owned()),
            resolution: None,
            bit_depth: None,
            hdr: None,
            channels: None,
            sampling_frequency_hz: None,
            default: false,
            forced: false,
            enabled: true,
        }
    }

    fn file(tracks: Vec<MediaTrack>) -> MediaFile {
        MediaFile {
            path: "episode.mkv".into(),
            original_file_name: None,
            watch_root: None,
            relative_path: None,
            fingerprint: FileFingerprint {
                path: "episode.mkv".into(),
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

    #[test]
    fn requires_the_external_format_to_match_the_embedded_codec() {
        assert!(subtitle_codec_matches(
            "SubRip/SRT",
            Some("S_TEXT/UTF8"),
            Path::new("Episode.eng.srt")
        ));
        assert!(!subtitle_codec_matches(
            "SubStationAlpha",
            Some("S_TEXT/ASS"),
            Path::new("Episode.eng.srt")
        ));
        assert!(subtitle_codec_matches(
            "SubStationAlpha",
            Some("S_TEXT/ASS"),
            Path::new("Episode.eng.ass")
        ));
    }

    #[test]
    fn requires_both_tracks_to_have_the_same_name() {
        assert!(subtitle_names_match(Some("Dialogue"), Some("dialogue")));
        assert!(subtitle_names_match(None, None));
        assert!(!subtitle_names_match(Some("Dialogue"), Some("SDH")));
        assert!(!subtitle_names_match(Some("Dialogue"), None));
    }

    #[test]
    fn maps_reversed_subtitles_to_the_template_order() {
        let template_tracks = [
            track(0, 1, TrackKind::Video, ""),
            track(1, 2, TrackKind::Subtitle, "Dialogue"),
            track(2, 3, TrackKind::Subtitle, "Signs/Songs"),
        ];
        let target = file(vec![
            track(0, 1, TrackKind::Video, ""),
            track(1, 2, TrackKind::Subtitle, "Signs/Songs"),
            track(2, 3, TrackKind::Subtitle, "Dialogue"),
        ]);
        let desired = template_tracks.iter().collect::<Vec<_>>();

        assert_eq!(track_order_for(&target, &desired), Ok(vec![0, 2, 1]));
    }

    #[test]
    fn refuses_to_guess_between_duplicate_track_signatures() {
        let template_tracks = [track(0, 1, TrackKind::Subtitle, "Dialogue")];
        let target = file(vec![
            track(1, 1, TrackKind::Subtitle, "Dialogue"),
            track(2, 2, TrackKind::Subtitle, "Dialogue"),
        ]);
        let desired = template_tracks.iter().collect::<Vec<_>>();

        assert!(track_order_for(&target, &desired).is_err());
    }

    #[test]
    fn changes_ass_fonts_without_changing_other_style_fields() {
        let source = "[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, OutlineColour, Outline, Bold\nStyle: Default,Arial,48,&H00FFFFFF,&H00000000,2,-1\n[Events]\nFormat: Layer, Start, End, Text\nDialogue: 0,0:00:01.00,0:00:02.00,{\\fnComic Sans}Hello, world\n";
        let result = transform_ass_document(
            source,
            "Noto Sans",
            "56",
            "&H0000FFFF",
            "&H00FF0000",
            "3.5",
            0,
        )
        .expect("transform");
        assert!(result.contains("Style: Default,Noto Sans,56,&H0000FFFF,&H00FF0000,3.5,-1"));
        assert!(result.contains("Dialogue: 0,0:00:01.00,0:00:02.00,{\\fnNoto Sans}Hello, world"));
    }

    #[test]
    fn converts_srt_to_styled_ass_and_preserves_shifted_timing() {
        let source = "1\r\n00:00:01,250 --> 00:00:03,500\r\n<i>Hello</i>\r\nworld\r\n";
        let result = transform_srt_document(
            source,
            "Andika Basic",
            "52",
            "&H00FFFFFF",
            "&H00000000",
            "2.5",
            500,
        )
        .expect("transform");
        assert!(result.contains("Style: Default,Andika Basic,52,&H00FFFFFF,&H00FFFFFF,&H00000000"));
        assert!(result.contains(
            "Dialogue: 0,0:00:01.75,0:00:04.00,Default,,0,0,0,,{\\i1}Hello{\\i0}\\Nworld"
        ));
    }

    #[test]
    fn subtitle_formatting_replaces_font_attachments_but_preserves_other_attachments() {
        let attachments = vec![
            MediaAttachment {
                id: 1,
                file_name: "Winter Storm.ttf".to_owned(),
                content_type: Some("application/x-truetype-font".to_owned()),
                description: None,
                size_bytes: Some(100),
            },
            MediaAttachment {
                id: 2,
                file_name: "Winter Storm.ttf".to_owned(),
                content_type: Some("application/x-truetype-font".to_owned()),
                description: None,
                size_bytes: Some(100),
            },
            MediaAttachment {
                id: 3,
                file_name: "cover.jpg".to_owned(),
                content_type: Some("image/jpeg".to_owned()),
                description: None,
                size_bytes: Some(200),
            },
        ];
        let mut arguments = Vec::new();

        let replaced = BTreeSet::from([font_match_key("Winter Storm")]);
        append_subtitle_font_attachment_selection(
            &mut arguments,
            &attachments,
            "AndikaBasic-Regular.ttf",
            Some(&replaced),
        );

        assert_eq!(arguments, ["--attachments", "3"]);
    }

    #[test]
    fn subtitle_formatting_disables_inherited_attachments_when_only_fonts_exist() {
        let attachments = vec![MediaAttachment {
            id: 7,
            file_name: "AndikaBasic-Regular.ttf".to_owned(),
            content_type: None,
            description: None,
            size_bytes: None,
        }];
        let mut arguments = Vec::new();

        append_subtitle_font_attachment_selection(
            &mut arguments,
            &attachments,
            "AndikaBasic-Regular.ttf",
            None,
        );

        assert_eq!(arguments, ["--no-attachments"]);
    }

    #[test]
    fn full_track_formatting_removes_old_fonts_even_when_file_and_family_names_differ() {
        let attachments = vec![
            MediaAttachment {
                id: 1,
                file_name: "Unrelated File Name.ttf".to_owned(),
                content_type: Some("application/x-truetype-font".to_owned()),
                description: None,
                size_bytes: None,
            },
            MediaAttachment {
                id: 2,
                file_name: "cover.jpg".to_owned(),
                content_type: Some("image/jpeg".to_owned()),
                description: None,
                size_bytes: None,
            },
        ];
        let mut arguments = Vec::new();
        let rewritten_fonts = BTreeSet::new();

        append_subtitle_font_attachment_selection(
            &mut arguments,
            &attachments,
            "Gandhi Sans Bold.otf",
            Some(&rewritten_fonts),
        );

        assert_eq!(arguments, ["--attachments", "2"]);
    }

    #[test]
    fn partial_track_formatting_keeps_unrelated_embedded_fonts() {
        let attachments = vec![
            MediaAttachment {
                id: 1,
                file_name: "Winter Storm.ttf".to_owned(),
                content_type: Some("application/x-truetype-font".to_owned()),
                description: None,
                size_bytes: None,
            },
            MediaAttachment {
                id: 2,
                file_name: "AndikaBasic-Regular.ttf".to_owned(),
                content_type: Some("application/x-truetype-font".to_owned()),
                description: None,
                size_bytes: None,
            },
        ];
        let mut arguments = Vec::new();

        append_subtitle_font_attachment_selection(
            &mut arguments,
            &attachments,
            "AndikaBasic-Regular.ttf",
            None,
        );

        assert_eq!(arguments, ["--attachments", "1"]);
    }

    #[test]
    fn finds_style_and_inline_fonts_replaced_in_an_ass_track() {
        let content = "[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Winter Storm,48\n[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\fnHey Comic}Hello";
        assert_eq!(
            ass_font_names(content),
            BTreeSet::from([font_match_key("Winter Storm"), font_match_key("Hey Comic")])
        );
    }

    #[test]
    fn shifts_ass_cues_and_clamps_negative_times() {
        assert_eq!(
            shift_ass_time("0:00:01.25", 500).expect("shift"),
            "0:00:01.75"
        );
        assert_eq!(
            shift_ass_time("0:00:01.25", -2_000).expect("shift"),
            "0:00:00.00"
        );
    }

    #[test]
    fn accepts_only_safe_supported_imported_fonts() {
        assert!(validate_imported_font("Papernotes.ttf", "AA==").is_ok());
        assert!(validate_imported_font("../Papernotes.ttf", "AA==").is_err());
        assert!(validate_imported_font("Papernotes.exe", "AA==").is_err());
        assert!(validate_imported_font("Papernotes.ttf", "not base64").is_err());
    }
}

#[cfg(test)]
pub(super) mod execution_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use mkvo_application::{
        FileSystem, MediaCatalog, MediaEnumerationRequest, MediaProbe, PortError,
        ToolExecutionResult, ToolExecutor, ToolRegistry,
    };
    use mkvo_contracts::{JobSnapshot, JobStatus, ToolStatus};
    use mkvo_infra_sqlite::{SqliteRepositories, SqliteStore};
    use mkvo_infra_watch::{AuthorizedRoot, AuthorizedRoots, LocalFileSystem};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{MemorySecretStore, RuntimeConfig, RuntimeDependencies};

    struct NoScan;

    #[async_trait]
    impl MediaCatalog for NoScan {
        async fn enumerate(
            &self,
            _request: &MediaEnumerationRequest,
            _cancel: CancellationToken,
        ) -> Result<Vec<FileFingerprint>, PortError> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl MediaProbe for NoScan {
        async fn inspect(
            &self,
            path: &Path,
            _cancel: CancellationToken,
        ) -> Result<MediaFile, PortError> {
            Err(PortError::NotFound(path.display().to_string()))
        }
    }

    struct InstalledTools;

    #[async_trait]
    impl ToolRegistry for InstalledTools {
        async fn status(&self, logical_name: &str) -> Result<ToolStatus, PortError> {
            Ok(ToolStatus {
                name: logical_name.to_owned(),
                command: logical_name.to_owned(),
                resolved_path: format!("/tools/{logical_name}"),
                available: true,
                version: "test".to_owned(),
                error: None,
            })
        }

        async fn all_statuses(&self) -> Result<Vec<ToolStatus>, PortError> {
            Ok(Vec::new())
        }
    }

    /// Stands in for MKVToolNix: every invocation runs the closure instead.
    pub struct ScriptedTools<F>(pub F);

    #[async_trait]
    impl<F> ToolExecutor for ScriptedTools<F>
    where
        F: Fn(&ToolInvocation) -> ToolExecutionResult + Send + Sync,
    {
        async fn execute(
            &self,
            invocation: &ToolInvocation,
            _cancel: CancellationToken,
        ) -> Result<ToolExecutionResult, PortError> {
            Ok((self.0)(invocation))
        }
    }

    /// What the process adapter reports for a tool that printed `stdout` and exited.
    pub fn exited(
        invocation: &ToolInvocation,
        exit_code: i32,
        stdout: &str,
    ) -> ToolExecutionResult {
        ToolExecutionResult {
            exit_code: Some(exit_code),
            stdout: stdout.to_owned(),
            stderr: String::new(),
            validated_outputs: invocation
                .expected_outputs
                .iter()
                .filter(|path| path.is_file())
                .cloned()
                .collect(),
        }
    }

    /// A runtime over a real temporary folder, with only the external tools faked.
    pub struct Library {
        pub runtime: MkvoRuntime,
        directory: tempfile::TempDir,
    }

    pub fn library(tools: impl ToolExecutor + 'static) -> Library {
        library_with(tools, |_| {})
    }

    /// Like `library`, after `adjust` swapped in whatever the test needs to misbehave.
    pub fn library_with(
        tools: impl ToolExecutor + 'static,
        adjust: impl FnOnce(&mut RuntimeDependencies),
    ) -> Library {
        let directory = tempfile::tempdir().expect("tempdir");
        let roots =
            AuthorizedRoots::new([AuthorizedRoot::new(directory.path(), true).expect("root")])
                .expect("roots");
        let repositories = Arc::new(SqliteRepositories::from_store(
            SqliteStore::open_in_memory().expect("SQLite"),
        ));
        let mut dependencies = RuntimeDependencies {
            catalog: Arc::new(NoScan),
            probe: Arc::new(NoScan),
            cache: repositories.clone(),
            paths: Arc::new(roots.clone()),
            file_system: Arc::new(LocalFileSystem::new(roots.clone(), false)),
            tools: Arc::new(InstalledTools),
            tool_executor: Arc::new(tools),
            settings: repositories.clone(),
            secrets: Arc::new(MemorySecretStore::new()),
            plans: repositories.clone(),
            jobs: repositories.clone(),
            rename_history: repositories.clone(),
            journal: repositories.clone(),
            logs: repositories,
            watcher: None,
            process_tools: None,
            authorized_roots: Some(roots),
        };
        adjust(&mut dependencies);
        let config = RuntimeConfig::new(directory.path(), directory.path());
        Library {
            runtime: MkvoRuntime::from_parts(config, dependencies),
            directory,
        }
    }

    impl Library {
        /// Writes a file and describes it as a scanned video with one removable audio track.
        pub async fn media(&self, name: &str) -> MediaFile {
            std::fs::write(self.directory.path().join(name), "original").expect("media");
            let fingerprint = self
                .runtime
                .dependencies()
                .file_system
                .fingerprint(&self.directory.path().join(name))
                .await
                .expect("fingerprint");
            let video = MediaTrack {
                mkvmerge_id: 0,
                propedit_track_number: 1,
                kind: TrackKind::Video,
                codec: "AVC".to_owned(),
                codec_id: None,
                language: None,
                name: None,
                resolution: None,
                bit_depth: None,
                hdr: None,
                channels: None,
                sampling_frequency_hz: None,
                default: true,
                forced: false,
                enabled: true,
            };
            let audio = MediaTrack {
                mkvmerge_id: 1,
                propedit_track_number: 2,
                kind: TrackKind::Audio,
                ..video.clone()
            };
            MediaFile {
                path: fingerprint.path.clone(),
                original_file_name: None,
                watch_root: None,
                relative_path: None,
                fingerprint,
                container: ContainerMetadata::default(),
                tracks: vec![video, audio],
                attachments: Vec::new(),
                episode: None,
                provider_match: None,
                status: MediaStatus::Ready,
            }
        }

        pub fn file_names(&self) -> Vec<String> {
            let mut names: Vec<_> = std::fs::read_dir(self.directory.path())
                .expect("directory")
                .map(|entry| {
                    entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        }

        pub async fn start_job<F, Fut>(&self, kind: JobKind, task: F) -> mkvo_domain::JobId
        where
            F: FnOnce(MkvoRuntime, mkvo_application::JobContext) -> Fut + Send + 'static,
            Fut: Future<Output = Result<JobCompletion, ApplicationError>> + Send + 'static,
        {
            let runtime = self.runtime.clone();
            self.runtime
                .jobs()
                .start(
                    JobSpec {
                        kind,
                        idempotency_key: IdempotencyKey::generate(),
                        request_fingerprint: "test".to_owned(),
                        plan_id: None,
                        total: 0,
                        resources: Vec::new(),
                    },
                    move |context| task(runtime, context),
                )
                .await
                .expect("job")
                .id
        }

        pub async fn finished_job(&self, id: mkvo_domain::JobId) -> JobSnapshot {
            loop {
                let snapshot = self
                    .runtime
                    .jobs()
                    .get(id)
                    .await
                    .expect("job")
                    .expect("snapshot");
                if snapshot.status.is_terminal() {
                    return snapshot;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }

        pub async fn run_remux(&self, plan: RemuxPlan) -> JobSnapshot {
            self.runtime.persist_plan(&plan).await.expect("persist");
            let id = self
                .start_job(JobKind::Remux, |runtime, context| async move {
                    runtime.execute_remux_plan(&plan, &context).await
                })
                .await;
            self.finished_job(id).await
        }
    }

    /// An in-place remux that drops the audio track of each file.
    fn remux_request(files: Vec<MediaFile>) -> RemuxPlanRequest {
        RemuxPlanRequest {
            mode: RemuxMode::Remux,
            files,
            options: RemuxOptions {
                remove_track_ids: BTreeSet::from([1]),
                ..RemuxOptions::default()
            },
            external_subtitles: BTreeMap::new(),
            extractions: BTreeMap::new(),
            existing_paths: BTreeSet::new(),
            source_access: BTreeMap::new(),
            authorized_roots: Vec::new(),
            settings_fingerprint: "settings".to_owned(),
            tool_fingerprints: BTreeMap::new(),
            expires_in_seconds: 60,
            idempotency_key: IdempotencyKey::generate(),
        }
    }

    fn remux_plan(files: Vec<MediaFile>) -> RemuxPlan {
        RemuxPlanner
            .build_plan(remux_request(files))
            .expect("remux plan")
    }

    fn writes_output(invocation: &ToolInvocation) -> ToolExecutionResult {
        for output in &invocation.expected_outputs {
            std::fs::write(output, "new").expect("output");
        }
        exited(invocation, 0, "")
    }

    const WARNED: &str = "mkvmerge v102.0 ('Little Houses') 64-bit\n'a.srt': Using the demultiplexer for the format 'SRT subtitles'.\nWarning: 'a.srt' track 0: The start timestamp is smaller than that of the previous entry.\nThe file has been opened for writing.\nProgress: 96%\rProgress: 100%\rThe cue entries (the index) are being written...\nMultiplexing took 0 seconds.\n";
    const FAILED: &str = "mkvmerge v102.0 ('Little Houses') 64-bit\nError: The file 'Episode 01.mkv' could not be opened for reading: open file error.\n";

    #[tokio::test]
    async fn output_written_with_warnings_replaces_the_original() {
        let library = library(ScriptedTools(|invocation: &ToolInvocation| {
            std::fs::write(&invocation.expected_outputs[0], "remuxed").expect("output");
            exited(invocation, 1, WARNED)
        }));
        let episode = library.media("Episode 01.mkv").await;

        let job = library.run_remux(remux_plan(vec![episode.clone()])).await;

        assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);
        assert_eq!(
            std::fs::read_to_string(&episode.path).expect("episode"),
            "remuxed"
        );
        assert_eq!(library.file_names(), ["Episode 01.mkv"]);
        assert!(job.lines.iter().any(|line| {
            line.contains("Warning: 'a.srt' track 0") && !line.contains("Progress")
        }));
    }

    #[tokio::test]
    async fn failed_remux_keeps_the_original_and_removes_the_partial_output() {
        let library = library(ScriptedTools(|invocation: &ToolInvocation| {
            std::fs::write(&invocation.expected_outputs[0], "partial").expect("output");
            exited(invocation, 2, FAILED)
        }));
        let episode = library.media("Episode 01.mkv").await;

        let job = library.run_remux(remux_plan(vec![episode.clone()])).await;

        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(
            std::fs::read_to_string(&episode.path).expect("episode"),
            "original"
        );
        assert_eq!(library.file_names(), ["Episode 01.mkv"]);
        let error = job.error.expect("error");
        assert!(error.contains("mkvmerge failed (exit code 2)"), "{error}");
        assert!(error.contains("could not be opened for reading"), "{error}");
    }

    #[tokio::test]
    async fn a_temporary_that_cannot_be_removed_is_only_a_warning() {
        let library = library(ScriptedTools(|invocation: &ToolInvocation| {
            let output = &invocation.expected_outputs[0];
            std::fs::write(output, "remuxed").expect("output");
            // A folder with content stands in for a file the platform refuses to delete.
            let font = output.with_file_name(".Episode 01.mkvo-font-font.ttf");
            std::fs::create_dir(&font).expect("font");
            std::fs::write(font.join("held"), "").expect("held");
            exited(invocation, 0, "")
        }));
        let episode = library.media("Episode 01.mkv").await;
        let mut plan = remux_plan(vec![episode.clone()]);
        plan.context
            .attributes
            .insert("subtitleFontFileName".to_owned(), "font.ttf".to_owned());

        let job = library.run_remux(plan).await;

        assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);
        assert_eq!(
            std::fs::read_to_string(&episode.path).expect("episode"),
            "remuxed"
        );
        assert!(
            job.lines
                .iter()
                .any(|line| line.contains("temporary file was left behind"))
        );
    }

    #[tokio::test]
    async fn no_argument_can_be_read_by_mkvmerge_as_an_options_file() {
        assert!(validate_imported_font("@fonts.ttf", "AA==").is_err());
        let library = library(ScriptedTools(|invocation: &ToolInvocation| {
            // mkvmerge loads any argument that starts with `@` as a file of further options.
            if invocation
                .arguments
                .iter()
                .any(|argument| argument.starts_with('@'))
            {
                exited(invocation, 2, "Error: an options file was read\n")
            } else {
                writes_output(invocation)
            }
        }));
        let episode = library.media("Episode 01.mkv").await;
        let mut plan = remux_plan(vec![episode]);
        plan.context
            .attributes
            .insert("subtitleFontFileName".to_owned(), "@fonts.ttf".to_owned());

        let job = library.run_remux(plan).await;

        assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);
    }

    #[tokio::test]
    async fn a_backup_left_by_an_older_version_does_not_block_the_remux() {
        let library = library(ScriptedTools(writes_output));
        let episode = library.media("Episode 01.mkv").await;
        let backup = episode.path.with_file_name(".Episode 01.mkv.mkvo-backup");
        std::fs::write(&backup, "older").expect("backup");

        let job = library.run_remux(remux_plan(vec![episode.clone()])).await;

        assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);
        assert_eq!(
            std::fs::read_to_string(&episode.path).expect("episode"),
            "new"
        );
        assert_eq!(std::fs::read_to_string(&backup).expect("backup"), "older");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn an_original_that_cannot_be_replaced_is_left_untouched() {
        use std::os::windows::fs::OpenOptionsExt;

        let library = library(ScriptedTools(writes_output));
        let episode = library.media("Episode 01.mkv").await;
        // A media server playing the file: others may read it, nobody may replace it.
        let playing = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&episode.path)
            .expect("open");

        let job = library.run_remux(remux_plan(vec![episode.clone()])).await;
        drop(playing);

        assert_eq!(job.status, JobStatus::Failed);
        assert!(job.error.expect("error").contains("could not replace"));
        assert_eq!(
            std::fs::read_to_string(&episode.path).expect("episode"),
            "original"
        );
        assert_eq!(library.file_names(), ["Episode 01.mkv"]);
    }

    /// Extraction of one track over the subtitle that is already there.
    fn overwriting_extraction_plan(episode: MediaFile, subtitle: &Path) -> RemuxPlan {
        std::fs::write(subtitle, "older").expect("subtitle");
        let mut plan = RemuxPlanner
            .build_plan(RemuxPlanRequest {
                mode: RemuxMode::ExtractSubtitles,
                extractions: BTreeMap::from([(
                    episode.path.clone(),
                    vec![TrackExtraction {
                        track_id: 2,
                        kind: TrackKind::Subtitle,
                        output: subtitle.to_path_buf(),
                    }],
                )]),
                ..remux_request(vec![episode])
            })
            .expect("extraction plan");
        plan.context.attributes.insert(
            "extractOverwriteExistingFiles".to_owned(),
            "true".to_owned(),
        );
        plan
    }

    /// A share where replacing a file removes the old one and then fails.
    struct LosesReplacedFile(Arc<dyn FileSystem>);

    #[async_trait]
    impl FileSystem for LosesReplacedFile {
        async fn exists(&self, path: &Path) -> Result<bool, PortError> {
            self.0.exists(path).await
        }

        async fn is_directory(&self, path: &Path) -> Result<bool, PortError> {
            self.0.is_directory(path).await
        }

        async fn fingerprint(&self, path: &Path) -> Result<FileFingerprint, PortError> {
            self.0.fingerprint(path).await
        }

        async fn move_file(&self, source: &Path, target: &Path) -> Result<(), PortError> {
            self.0.move_file(source, target).await
        }

        async fn remove_file(&self, path: &Path) -> Result<(), PortError> {
            self.0.remove_file(path).await
        }

        async fn probe_access(
            &self,
            path: &Path,
            access: RequiredAccess,
        ) -> Result<FileAccessState, PortError> {
            self.0.probe_access(path, access).await
        }

        async fn replace_file(&self, _source: &Path, target: &Path) -> Result<(), PortError> {
            self.0.remove_file(target).await?;
            Err(PortError::Other("the share went away".to_owned()))
        }
    }

    fn losing_replaced_files(dependencies: &mut RuntimeDependencies) {
        dependencies.file_system = Arc::new(LosesReplacedFile(dependencies.file_system.clone()));
    }

    #[tokio::test]
    async fn a_replace_that_loses_the_original_keeps_the_new_file() {
        let library = library_with(ScriptedTools(writes_output), losing_replaced_files);
        let episode = library.media("Episode 01.mkv").await;
        let kept = episode
            .path
            .with_file_name(".Episode 01.mkv.mkvo-remux.tmp");

        let job = library.run_remux(remux_plan(vec![episode])).await;

        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(std::fs::read_to_string(&kept).expect("kept"), "new");
        let error = job.error.expect("error");
        assert!(error.contains("the share went away"), "{error}");
        assert!(
            error.contains("was kept at") && error.contains(".Episode 01.mkv.mkvo-remux.tmp"),
            "{error}"
        );
        assert!(job.lines.iter().any(|line| line.contains("was kept at")));
    }

    #[tokio::test]
    async fn an_extraction_that_loses_the_old_subtitle_keeps_the_new_one() {
        let library = library_with(ScriptedTools(writes_output), losing_replaced_files);
        let episode = library.media("Episode 01.mkv").await;
        let subtitle = episode.path.with_file_name("Episode 01.eng.srt");
        let kept = subtitle.with_file_name(".Episode 01.eng.srt.mkvo-extract-2.tmp");

        let job = library
            .run_remux(overwriting_extraction_plan(episode, &subtitle))
            .await;

        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(std::fs::read_to_string(&kept).expect("kept"), "new");
        assert!(job.error.expect("error").contains("was kept at"));
    }

    #[tokio::test]
    async fn extraction_replaces_an_existing_subtitle_without_a_backup() {
        let library = library(ScriptedTools(writes_output));
        let episode = library.media("Episode 01.mkv").await;
        let subtitle = episode.path.with_file_name("Episode 01.eng.srt");

        let job = library
            .run_remux(overwriting_extraction_plan(episode, &subtitle))
            .await;

        assert_eq!(job.status, JobStatus::Completed, "{:?}", job.error);
        assert_eq!(std::fs::read_to_string(&subtitle).expect("subtitle"), "new");
        assert_eq!(
            library.file_names(),
            ["Episode 01.eng.srt", "Episode 01.mkv"]
        );
    }

    #[test]
    fn a_batch_failure_names_the_failed_files_and_ignores_canceled_ones() {
        let failed = |name: &str| {
            (
                format!("/media/{name}"),
                ApplicationError::Internal("tool failed".to_owned()),
            )
        };
        let canceled = || ("/media/c.mkv".to_owned(), ApplicationError::Canceled);

        let only = batch_failure(vec![failed("a.mkv")], 1);
        assert_eq!(only.to_string(), "internal error: tool failed");
        let mixed = batch_failure(vec![canceled(), failed("a.mkv"), failed("b.mkv")], 4);
        assert_eq!(
            mixed.to_string(),
            "internal error: 2 of 4 files failed: a.mkv: tool failed; b.mkv: tool failed"
        );
        let many = batch_failure(
            (0..7)
                .map(|index| failed(&format!("{index}.mkv")))
                .collect(),
            9,
        );
        assert!(
            many.to_string()
                .starts_with("internal error: 7 of 9 files failed: 0.mkv: ")
        );
        assert!(
            many.to_string()
                .ends_with("; and 2 more, listed in the job log")
        );
        assert!(matches!(
            batch_failure(vec![canceled(), canceled()], 4),
            ApplicationError::Canceled
        ));
    }

    #[test]
    fn tool_messages_come_from_stdout_when_stderr_is_empty() {
        let invocation = ToolInvocation {
            tool: "mkvmerge".to_owned(),
            executable: PathBuf::from("mkvmerge"),
            arguments: Vec::new(),
            working_directory: None,
            expected_outputs: Vec::new(),
        };

        let clean = tool_warning("mkvmerge", &exited(&invocation, 0, WARNED)).expect("clean");
        assert_eq!(clean, None);
        let warned = tool_warning("mkvmerge", &exited(&invocation, 1, WARNED)).expect("warned");
        assert_eq!(
            warned.as_deref(),
            Some(
                "mkvmerge finished with warnings: Warning: 'a.srt' track 0: The start timestamp is smaller than that of the previous entry."
            )
        );
        let unlabelled =
            tool_warning("mkvmerge", &exited(&invocation, 2, "one\ntwo\r\n")).expect_err("failed");
        assert_eq!(
            unlabelled.to_string(),
            "internal error: mkvmerge failed (exit code 2): one | two"
        );
        let mut on_stderr = exited(&invocation, 2, FAILED);
        on_stderr.stderr = "1\n2\n3\n4\n5\n6\naccess denied\n".to_owned();
        let on_stderr = tool_warning("mkvmerge", &on_stderr).expect_err("failed");
        assert!(
            on_stderr
                .to_string()
                .ends_with("(exit code 2): 3 | 4 | 5 | 6 | access denied")
        );
    }

    // What `taskkill /F` left of a real mkvmerge v102 run: exit code 1 and a third of the file.
    const KILLED: &str =
        "mkvmerge v102.0 ('Little Houses') 64-bit\nProgress: 0%\rProgress: 10%\rProgress: 35%\r";

    #[tokio::test]
    async fn a_killed_mkvmerge_is_not_taken_for_one_that_finished_with_warnings() {
        let library = library(ScriptedTools(|invocation: &ToolInvocation| {
            std::fs::write(&invocation.expected_outputs[0], "truncated").expect("output");
            exited(invocation, 1, KILLED)
        }));
        let episode = library.media("Episode 01.mkv").await;

        let job = library.run_remux(remux_plan(vec![episode.clone()])).await;

        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(
            std::fs::read_to_string(&episode.path).expect("episode"),
            "original"
        );
        assert_eq!(library.file_names(), ["Episode 01.mkv"]);
        let error = job.error.expect("error");
        assert!(
            error.contains("mkvmerge did not finish (exit code 1)"),
            "{error}"
        );
    }

    #[test]
    fn exit_code_one_counts_only_with_the_line_a_finished_tool_prints_last() {
        let ended = |tool: &str, stdout: &str| {
            let execution = mkvo_application::ToolExecutionResult {
                exit_code: Some(1),
                stdout: stdout.to_owned(),
                stderr: String::new(),
                validated_outputs: Vec::new(),
            };
            tool_warning(tool, &execution).is_ok()
        };

        assert!(ended("mkvmerge", WARNED));
        assert!(ended(
            "mkvmerge",
            "Multiplexing took 1 minute 5 seconds.\r\n"
        ));
        assert!(!ended("mkvmerge", KILLED));
        assert!(ended(
            "mkvpropedit",
            "Warning: x\nThe changes are written to the file.\nDone.\n"
        ));
        assert!(ended("mkvpropedit", "Warning: x\nNo changes were made.\n"));
        assert!(!ended(
            "mkvpropedit",
            "The file is being analyzed.\nThe changes are written to the file.\n"
        ));
        assert!(!ended("mkvextract", "Progress: 100%\n"));
    }
}
