use crate::ai::AiProvider;
use crate::db::{AttributedSubsystem, Database, SubsystemSource};
use crate::toolbox::ToolBox;
use crate::workflows::linux_bug::BugInput;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

/// How long a claim stays valid without being renewed.
///
/// This is deliberately far shorter than an analysis takes. The worker renews
/// the lease while it works, so the value only has to outlast a renewal
/// interval, and a worker that dies is noticed in minutes rather than after a
/// whole pipeline's worth of time.
const BUG_LEASE_TTL_SECONDS: i64 = 300;
/// How often a running analysis pushes its lease forward. Comfortably shorter
/// than the lease so that one failed renewal does not forfeit the claim.
const BUG_LEASE_RENEW_INTERVAL_SECONDS: u64 = 60;
const BUG_MAX_ATTEMPTS: i64 = 3;

pub struct BugWorker {
    db: Arc<Database>,
    provider: Arc<dyn AiProvider>,
    repo_path: String,
    /// Identifies this worker in the lease it takes, so that a lease which
    /// never gets released can be traced back to a process.
    worker_id: String,
}

impl BugWorker {
    pub fn new(db: Arc<Database>, provider: Arc<dyn AiProvider>, repo_path: String) -> Self {
        Self {
            db,
            provider,
            repo_path,
            worker_id: format!(
                "{}:{}",
                std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown-host".to_string()),
                std::process::id()
            ),
        }
    }

    pub async fn run(&self) {
        info!(
            "Starting Bug Worker as {} (lease {}s renewed every {}s, {} attempts max)...",
            self.worker_id,
            BUG_LEASE_TTL_SECONDS,
            BUG_LEASE_RENEW_INTERVAL_SECONDS,
            BUG_MAX_ATTEMPTS
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
                .claim_pending_bug(&self.worker_id, BUG_LEASE_TTL_SECONDS, BUG_MAX_ATTEMPTS)
                .await
            {
                Ok(Some(bug)) => {
                    let provider = self.provider.clone();
                    let db = self.db.clone();
                    let repo_path = self.repo_path.clone();
                    let worker_id = self.worker_id.clone();
                    tokio::spawn(async move {
                        let actor = if !bug.reporter.is_empty() {
                            bug.reporter.as_str()
                        } else {
                            "sashiko"
                        };
                        // A short lease only survives while something keeps
                        // pushing it forward. Renewing for as long as the
                        // analysis runs is what stops a pipeline that outlives
                        // the lease from being claimed and run a second time.
                        let heartbeat_db = db.clone();
                        let heartbeat_worker = worker_id.clone();
                        let heartbeat_bug = bug.id;
                        let heartbeat = tokio::spawn(async move {
                            let mut ticker = tokio::time::interval(Duration::from_secs(
                                BUG_LEASE_RENEW_INTERVAL_SECONDS,
                            ));
                            // Interval fires once immediately; the claim is
                            // already fresh, so that tick is spent here.
                            ticker.tick().await;
                            loop {
                                ticker.tick().await;
                                match heartbeat_db
                                    .renew_bug_lease(
                                        heartbeat_bug,
                                        &heartbeat_worker,
                                        BUG_LEASE_TTL_SECONDS,
                                    )
                                    .await
                                {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        warn!(
                                            "Lease on bug {} is no longer held by {}; \
                                             stopping renewal",
                                            heartbeat_bug, heartbeat_worker
                                        );
                                        break;
                                    }
                                    // A single failed renewal is survivable
                                    // because the lease outlasts several
                                    // intervals, so keep trying.
                                    Err(e) => error!(
                                        "Failed to renew the lease on bug {}: {}",
                                        heartbeat_bug, e
                                    ),
                                }
                            }
                        });

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
                            // The stored bug keeps the subsystem names but the
                            // read model drops their provenance, so they are
                            // rebuilt as caller supplied. Nothing is lost: the
                            // analysis re-resolves subsystems from MAINTAINERS
                            // before the outcome is written back.
                            subsystems: bug
                                .subsystems
                                .iter()
                                .map(|name| {
                                    AttributedSubsystem::new(name, SubsystemSource::CallerSupplied)
                                })
                                .collect(),
                            source_files: bug.source_files().unwrap_or_default(),
                            commit_sha: bug.discovered_in_commit.clone(),
                            patchset_id: bug.discovered_in_patchset_id,
                            patch_id: bug.discovered_in_patch_id,
                            baseline_sha: bug.discovered_in_commit.clone(),
                            review_id: None,
                        });

                        let mut tb = ToolBox::new(std::path::PathBuf::from(&repo_path), None);
                        if let Some(ref sha) = bug.discovered_in_commit {
                            tb.set_virtual_head(sha.clone());
                        }
                        let tools = Some(Arc::new(tb));

                        let analysis = crate::workflows::linux_bug::process_issue_worker(
                            provider.as_ref(),
                            tools,
                            &db,
                            &bug,
                            input,
                            Some("bug_worker"),
                        )
                        .await;

                        // Renewal stops before the claim is settled. A renewal
                        // still in flight is harmless because it matches on
                        // locked_by, which both paths below clear, so it can
                        // only be a no-op once they have run.
                        heartbeat.abort();

                        match analysis {
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
                    if let Err(e) = self.db.abandon_exhausted_bugs(BUG_MAX_ATTEMPTS).await {
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
