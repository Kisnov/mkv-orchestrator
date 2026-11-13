//! Tauri delivery adapter for MKV Orchestrator.

mod commands;
mod error;
mod state;

use std::sync::Arc;

use mkvo_runtime::{MkvoRuntime, RuntimeError, RuntimeResult};
use tauri::Manager;
use tokio::sync::watch;

use crate::error::CommandError;

type Startup = watch::Receiver<Option<RuntimeResult<()>>>;

/// Wait until migration and crash recovery have finished, and say how.
async fn startup_finished(startup: &mut Startup) -> RuntimeResult<()> {
    match startup.wait_for(Option::is_some).await.as_deref() {
        Ok(Some(result)) => result.clone(),
        _ => Err(RuntimeError::internal("desktop startup did not finish")),
    }
}

async fn migrate_and_recover(runtime: &MkvoRuntime) -> RuntimeResult<()> {
    let migration = runtime.migrate_legacy_data().await?;
    tracing::info!(status = ?migration.status, "legacy data migration checked");
    let recovery = runtime.recover_startup_state().await?;
    tracing::info!(
        completed = recovery.completed,
        clean_retry = recovery.clean_retry,
        manual_review = recovery.manual_review,
        "startup recovery completed"
    );
    Ok(())
}

/// Starts the desktop host.
///
/// Feature commands and state composition are intentionally kept in this host;
/// media behavior remains in the shared application and infrastructure crates.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .try_init();

    let (startup_done, startup) = watch::channel(None);
    let commands: Arc<tauri::ipc::InvokeHandler<tauri::Wry>> = Arc::new(tauri::generate_handler![
        commands::get_status,
        commands::browse_file_system,
        commands::start_scan,
        commands::get_scan_job,
        commands::cancel_scan,
        commands::get_current_scan_files,
        commands::clear_current_scan_files,
        commands::set_file_selection,
        commands::authorize_browsed_root,
        commands::get_web_settings,
        commands::save_web_settings,
        commands::test_media_server_connection,
        commands::sync_media_server_libraries,
        commands::search_rename_metadata,
        commands::load_rename_scopes,
        commands::test_rename_provider,
        commands::build_rename_preview,
        commands::apply_rename_preview,
        commands::get_rename_batches,
        commands::preview_rename_batch_undo,
        commands::undo_rename_batch,
        commands::clear_rename_batches,
        commands::build_mux_preview,
        commands::apply_mux_preview,
        commands::get_operation_job,
        commands::cancel_operation_job,
        commands::load_propedit_template,
        commands::build_propedit_preview,
        commands::apply_propedit_preview,
        commands::run_library_audit,
        commands::get_library_catalog,
        commands::get_library_artwork,
        commands::get_library_local_artwork,
        commands::select_source_folder,
        commands::get_watch_health,
        commands::list_recent_jobs,
        commands::export_logs,
        commands::get_logs,
        commands::clear_logs,
    ]);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let runtime = state::compose_runtime(app.handle())?;
            app.manage(Arc::clone(&runtime));
            // Migration and crash recovery run off the event loop so the window
            // can draw meanwhile. Every command waits for them: a usable OS
            // keychain selects the async migration path, and the first
            // settings read must not race the one-time import.
            tauri::async_runtime::spawn(async move {
                let started = migrate_and_recover(&runtime).await;
                if let Err(error) = &started {
                    tracing::error!(%error, "desktop startup failed");
                }
                let ready = started.is_ok();
                let _ = startup_done.send(Some(started));
                if !ready {
                    return;
                }
                // Watching enumerates configured roots, which on a network
                // share can block, so nothing waits on it. A watcher that
                // cannot start is logged, never fatal.
                match runtime.start_watchers().await {
                    Ok(report) if report.started => {
                        tracing::info!(roots = report.roots.len(), "watch folders active");
                    }
                    Ok(report) => {
                        if let Some(error) = report.error {
                            tracing::warn!(%error, "watch folders inactive");
                        }
                    }
                    Err(error) => tracing::warn!(%error, "watch folders could not be started"),
                }
            });
            Ok(())
        })
        .invoke_handler(move |invoke| {
            if matches!(*startup.borrow(), Some(Ok(()))) {
                return commands(invoke);
            }
            let commands = Arc::clone(&commands);
            let mut startup = startup.clone();
            tauri::async_runtime::spawn(async move {
                let resolver = invoke.resolver.clone();
                let command = invoke.message.command().to_owned();
                match startup_finished(&mut startup).await {
                    Ok(()) => {
                        if !commands(invoke) {
                            resolver.reject(format!("Command {command} not found"));
                        }
                    }
                    Err(error) => resolver.reject(CommandError::from(error)),
                }
            });
            true
        })
        .run(tauri::generate_context!())
        .expect("failed to start MKV Orchestrator");
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn commands_wait_for_startup_to_finish() {
        let (done, mut startup) = watch::channel(None);
        let waiting = tokio::spawn(async move { startup_finished(&mut startup).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());

        done.send(Some(Ok(()))).unwrap();
        assert_eq!(waiting.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn commands_see_why_startup_failed() {
        let failure = RuntimeError::internal("migration failed");
        let (_done, mut startup) = watch::channel(Some(Err(failure.clone())));
        assert_eq!(startup_finished(&mut startup).await, Err(failure));

        // The startup task ended without saying how.
        let (_, mut abandoned) = watch::channel::<Option<RuntimeResult<()>>>(None);
        assert!(startup_finished(&mut abandoned).await.is_err());
    }
}
