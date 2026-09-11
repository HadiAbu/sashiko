use crate::ai::AiProvider;
use crate::db::Database;
use crate::toolbox::ToolBox;
use crate::workflows::linux_bug::BugInput;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info};

pub struct BugWorker {
    db: Arc<Database>,
    provider: Arc<dyn AiProvider>,
    repo_path: String,
    /// Identifies this worker in the lease it takes, so that a lease which
    /// never gets released can be traced back to a process.
    worker_id: String,
    lease_ttl_seconds: i64,
    max_attempts: i64,
}

impl BugWorker {
    pub fn new(
        db: Arc<Database>,
        provider: Arc<dyn AiProvider>,
        repo_path: String,
        settings: &crate::settings::LinuxBugSettings,
    ) -> Self {
        Self {
            db,
            provider,
            repo_path,
            worker_id: format!(
                "{}:{}",
                std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown-host".to_string()),
                std::process::id()
            ),
            lease_ttl_seconds: settings.lease_ttl_seconds,
            max_attempts: settings.max_attempts,
        }
    }

    pub async fn run(&self) {
        info!(
            "Starting Bug Worker as {} (lease {}s, {} attempts max)...",
            self.worker_id, self.lease_ttl_seconds, self.max_attempts
        );
        if let Err(e) = self.db.recover_stale_running_bugs().await {
            error!(
                "Failed to requeue interrupted bug analyses on startup: {}",
                e
            );
        }
        loop {
            match self
                .db
                .claim_pending_bug(&self.worker_id, self.lease_ttl_seconds, self.max_attempts)
                .await
            {
                Ok(Some(bug)) => {
                    let provider = self.provider.clone();
                    let db = self.db.clone();
                    let repo_path = self.repo_path.clone();
                    tokio::spawn(async move {
                        let actor = if !bug.reporter.is_empty() {
                            bug.reporter.as_str()
                        } else {
                            "sashiko"
                        };
                        let db = db.with_bug_actor(
                            actor,
                            "sashiko:linux_bug",
                            Some(provider.get_capabilities().model_name),
                        );
                        info!("Processing raw bug ID {} ({})", bug.id, bug.bugid);

                        let input = if let Some(raw) = bug.raw_input() {
                            serde_json::from_str::<BugInput>(&raw).ok()
                        } else {
                            None
                        }
                        .unwrap_or_else(|| BugInput {
                            problem: bug.problem().to_string(),
                            reasoning: bug
                                .severity_explanation()
                                .unwrap_or_else(|| "No reasoning provided.".to_string()),
                            locations: bug.locations(),
                            subsystems: bug.subsystems.clone(),
                            source_files: bug.source_files().unwrap_or_default(),
                            commit_sha: bug.discovered_in_commit.clone(),
                            patchset_id: bug.discovered_in_patchset_id,
                            patch_id: bug.discovered_in_patch_id,
                            baseline_sha: bug.discovered_in_commit.clone(),
                        });

                        let mut tb = ToolBox::new(std::path::PathBuf::from(&repo_path), None);
                        if let Some(ref sha) = bug.discovered_in_commit {
                            tb.set_virtual_head(sha.clone());
                        }
                        let tools = Some(Arc::new(tb));

                        match crate::workflows::linux_bug::process_issue_worker(
                            provider.as_ref(),
                            tools,
                            &db,
                            &bug,
                            input,
                            Some("bug_worker"),
                        )
                        .await
                        {
                            Ok(outcome) => {
                                info!("Successfully processed raw bug {}: {}", bug.id, outcome);
                                // The workflow records the outcome; this only
                                // drops the claim so the row stops looking
                                // like it is still being worked on.
                                if let Err(e) = db.release_bug_lease(bug.id).await {
                                    error!("Failed to release lease on bug {}: {}", bug.id, e);
                                }
                            }
                            Err(e) => {
                                error!("Failed to process bug {}: {}", bug.id, e);
                                let error_msg = format!("Error during async processing: {}", e);
                                let _ = db.fail_bug_analysis(bug.id, &error_msg).await;
                            }
                        }
                    });
                }
                Ok(None) => {
                    // Nothing left to claim, so this is the cheapest moment to
                    // retire the bugs that have run out of attempts.
                    if let Err(e) = self.db.abandon_exhausted_bugs(self.max_attempts).await {
                        error!("Failed to abandon exhausted bugs: {}", e);
                    }
                    sleep(Duration::from_secs(5)).await;
                }
                Err(e) => {
                    error!("Database error while claiming a bug for analysis: {}", e);
                    sleep(Duration::from_secs(10)).await;
                }
            }
        }
    }
}
