use crate::git_ops::ensure_remote;
use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

pub struct GitSyncWorker {
    repo_path: PathBuf,
}

impl GitSyncWorker {
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }

    pub async fn run(&self) {
        info!("GitSyncWorker started. Will sync remotes periodically.");
        loop {
            if let Err(e) = self.sync_all_remotes().await {
                error!("GitSyncWorker failed during sync cycle: {}", e);
            }
            // Sleep for 1 hour before checking again.
            // ensure_remote handles the fine-grained 4h/24h timestamp logic.
            sleep(Duration::from_secs(3600)).await;
        }
    }

    async fn sync_all_remotes(&self) -> Result<()> {
        info!("GitSyncWorker: Starting sync cycle.");

        // Enumerate all configured remotes
        let output = crate::git_cmd::in_dir_async(&self.repo_path)
            .args(["remote"])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("GitSyncWorker: Failed to list git remotes: {}", stderr);
            return Err(anyhow::anyhow!("Failed to list remotes"));
        }

        let remotes_str = String::from_utf8_lossy(&output.stdout);
        let remotes: Vec<&str> = remotes_str
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("fetcher-"))
            .collect();

        info!("GitSyncWorker: Found {} remotes to check.", remotes.len());

        for remote in remotes {
            // Get URL for the remote
            let url_output = crate::git_cmd::in_dir_async(&self.repo_path)
                .args(["remote", "get-url", remote])
                .output()
                .await?;

            if !url_output.status.success() {
                warn!(
                    "GitSyncWorker: Failed to get URL for remote {}. Skipping.",
                    remote
                );
                continue;
            }

            let url = String::from_utf8_lossy(&url_output.stdout)
                .trim()
                .to_string();

            // Check if it's time to fetch and fetch if necessary
            // force_fetch=false so we respect the 4h/24h intervals in ensure_remote
            if let Err(e) = ensure_remote(&self.repo_path, remote, &url, false).await {
                error!("GitSyncWorker: Failed to sync remote {}: {}", remote, e);
            }
        }

        info!("GitSyncWorker: Sync cycle complete.");
        Ok(())
    }
}
