// Copyright 2026 The Sashiko Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::db::Database;
use crate::events::{Event, MessageSource};
use crate::fetcher::FetchRequest;
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Redirect},
    routing::{get, get_service, post},
};
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{error, info, warn};

use std::time::{Duration, Instant};
use tokio::sync::RwLock;

struct CachedValue<T> {
    value: T,
    timestamp: Instant,
}

struct AsyncCache<T> {
    inner: RwLock<Option<CachedValue<T>>>,
    ttl: Duration,
}

impl<T: Clone> AsyncCache<T> {
    fn new(ttl: Duration) -> Self {
        Self {
            inner: RwLock::new(None),
            ttl,
        }
    }

    async fn get_or_fetch<F, Fut, E>(&self, fetch: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        if let Some(cached) = self.inner.read().await.as_ref()
            && cached.timestamp.elapsed() < self.ttl
        {
            return Ok(cached.value.clone());
        }

        let mut write_guard = self.inner.write().await;
        if let Some(cached) = write_guard.as_ref()
            && cached.timestamp.elapsed() < self.ttl
        {
            return Ok(cached.value.clone());
        }

        let value = fetch().await?;
        *write_guard = Some(CachedValue {
            value: value.clone(),
            timestamp: Instant::now(),
        });
        Ok(value)
    }
}

struct AsyncMapCache<K, V> {
    inner: RwLock<std::collections::HashMap<K, CachedValue<V>>>,
    ttl: Duration,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> AsyncMapCache<K, V> {
    fn new(ttl: Duration) -> Self {
        Self {
            inner: RwLock::new(std::collections::HashMap::new()),
            ttl,
        }
    }

    async fn get_or_fetch<F, Fut, E>(&self, key: K, fetch: F) -> Result<V, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<V, E>>,
    {
        if let Some(cached) = self.inner.read().await.get(&key)
            && cached.timestamp.elapsed() < self.ttl
        {
            return Ok(cached.value.clone());
        }

        let mut write_guard = self.inner.write().await;
        if let Some(cached) = write_guard.get(&key)
            && cached.timestamp.elapsed() < self.ttl
        {
            return Ok(cached.value.clone());
        }

        let value = fetch().await?;
        write_guard.insert(
            key,
            CachedValue {
                value: value.clone(),
                timestamp: Instant::now(),
            },
        );
        Ok(value)
    }
}

pub struct AppState {
    pub settings: Arc<crate::settings::Settings>,
    pub db: Arc<Database>,
    pub sender: mpsc::Sender<Event>,
    pub fetch_sender: mpsc::Sender<FetchRequest>,
    pub forge_registry: Arc<crate::forge::ForgeRegistry>,
    pub read_only: bool,
    pub allow_all_submit: bool,
    pub smtp_enabled: bool,
    pub dry_run: bool,
    stats_timeline_cache: AsyncMapCache<Option<i64>, serde_json::Value>,
    stats_reviews_cache: AsyncCache<serde_json::Value>,
    stats_tools_cache: AsyncCache<serde_json::Value>,
    messages_count_cache: AsyncCache<usize>,
    patchsets_count_cache: AsyncCache<usize>,
    patchsets_homepage_cache: AsyncCache<Vec<crate::db::PatchsetRow>>,
    messages_homepage_cache: AsyncCache<Vec<crate::db::MessageRow>>,
    bug_subsystems_cache: AsyncMapCache<Option<String>, Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
pub struct Pagination {
    pub page: Option<usize>,
    pub per_page: Option<usize>,
    pub q: Option<String>,
    pub mailing_list: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PatchsetsResponse {
    pub items: Vec<crate::db::PatchsetRow>,
    pub total: usize,
    pub page: usize,
    pub per_page: usize,
}

#[derive(Serialize, Deserialize)]
pub struct MessagesResponse {
    pub items: Vec<crate::db::MessageRow>,
    pub total: usize,
    pub page: usize,
    pub per_page: usize,
}

#[derive(Deserialize)]
pub struct PatchQuery {
    pub id: String,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Deserialize)]
pub struct ReviewQuery {
    pub id: Option<i64>,
    pub patchset_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct BugQuery {
    pub id: Option<i64>,
    pub bugid: Option<String>,
    pub slug: Option<String>,
}

#[derive(Deserialize)]
pub struct BugListQuery {
    pub page: Option<usize>,
    pub per_page: Option<usize>,
    pub q: Option<String>,
    pub subsystem: Option<String>,
    pub subsystems: Option<String>,
    pub min_severity: Option<String>,
    pub severity: Option<String>,
    /// Triage state. Also accepts the historical `status` spelling.
    #[serde(alias = "status")]
    pub lifecycle_status: Option<String>,
    /// Analysis execution state.
    pub pipeline_state: Option<String>,
    /// Filters by assignee. The literal `none` selects unassigned bugs.
    pub assignee: Option<String>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
}

#[derive(Deserialize)]
pub struct BugSubsystemsQuery {
    #[serde(alias = "status")]
    pub lifecycle_status: Option<String>,
}

#[derive(Deserialize)]
pub struct RerunPatchQuery {
    pub patchset_id: i64,
    pub patch_id: i64,
}

#[derive(Deserialize)]
pub struct SubsystemQuery {
    pub subsystem_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct CancelQuery {
    pub id: i64,
    #[serde(default)]
    pub force: bool,
}

#[derive(Deserialize)]
pub struct InjectRequest {
    pub raw: String,
    pub group: Option<String>,
    pub baseline: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SubmitRequest {
    Inject {
        raw: String,
        base_commit: Option<String>,
        skip_subjects: Option<Vec<String>>,
        only_subjects: Option<Vec<String>>,
    },
    Remote {
        sha: String,
        repo: Option<String>,
        skip_subjects: Option<Vec<String>>,
        only_subjects: Option<Vec<String>>,
    },
    #[serde(rename = "remote-range")]
    RemoteRange {
        sha: String,
        repo: Option<String>,
        skip_subjects: Option<Vec<String>>,
        only_subjects: Option<Vec<String>>,
    },
    Thread {
        msgid: String,
    },
}

#[derive(Serialize, Deserialize)]
pub struct SubmitResponse {
    pub status: String,
    pub id: String,
}

async fn redirect_www(req: Request, next: Next) -> impl IntoResponse {
    if let Some(host) = req.headers().get("host").and_then(|h| h.to_str().ok()) {
        let host_without_port = host.split(':').next().unwrap_or("");
        if host_without_port == "www.sashiko.dev" {
            let uri = req.uri();
            let new_uri = format!(
                "https://sashiko.dev{}{}",
                uri.path(),
                uri.query().map(|q| format!("?{}", q)).unwrap_or_default()
            );
            return Redirect::permanent(&new_uri).into_response();
        }
    }
    next.run(req).await
}

/// Build the API router with all routes and shared state.
///
/// Extracted from [`run_server`] so that integration tests can construct the
/// router independently (e.g. bind to port 0 for random-port testing).
pub fn build_router(
    settings: Arc<crate::settings::Settings>,
    db: Arc<Database>,
    sender: mpsc::Sender<Event>,
    fetch_sender: mpsc::Sender<FetchRequest>,
    allow_all_submit: bool,
    smtp_enabled: bool,
    dry_run: bool,
) -> Router {
    let forge_registry = Arc::new(crate::forge::ForgeRegistry::new());
    let read_only = settings.server.read_only;

    let state = Arc::new(AppState {
        settings: settings.clone(),
        db,
        sender,
        fetch_sender,
        read_only,
        forge_registry,
        allow_all_submit,
        smtp_enabled,
        dry_run,
        stats_timeline_cache: AsyncMapCache::new(Duration::from_secs(60)),
        stats_reviews_cache: AsyncCache::new(Duration::from_secs(60)),
        stats_tools_cache: AsyncCache::new(Duration::from_secs(60)),
        messages_count_cache: AsyncCache::new(Duration::from_secs(30)),
        patchsets_count_cache: AsyncCache::new(Duration::from_secs(30)),
        patchsets_homepage_cache: AsyncCache::new(Duration::from_secs(10)),
        messages_homepage_cache: AsyncCache::new(Duration::from_secs(10)),
        bug_subsystems_cache: AsyncMapCache::new(Duration::from_secs(5)),
    });

    Router::new()
        .route("/health", get(health_check))
        .route("/api/config", get(get_config))
        .route("/api/lists", get(list_mailing_lists))
        .route("/api/patchsets", get(list_patchsets))
        .route("/api/messages", get(list_messages))
        .route("/api/patch", get(get_patchset))
        .route("/api/patchset", get(get_patchset_summary))
        .route("/api/message", get(get_message))
        .route("/api/review", get(get_review))
        .route("/api/review_log", get(get_review_log))
        .route("/api/stats", get(get_stats))
        .route("/api/stats/timeline", get(stats_timeline))
        .route("/api/stats/reviews", get(stats_reviews))
        .route("/api/stats/tools", get(stats_tools))
        .route("/api/submit", post(submit_patch))
        .route("/api/auth/request-link", post(request_link))
        .route("/api/auth/verify", get(verify_link))
        .route("/api/auth/refresh", post(refresh_token))
        .route("/api/patchset/rerun", post(rerun_patchset))
        .route("/api/patchset/cancel", post(cancel_patchset))
        .route("/api/patch/rerun", post(rerun_patch))
        .route("/api/bug", get(get_bug))
        .route("/api/bugs", get(list_bugs))
        .route("/api/bugs/subsystems", get(list_bug_subsystems))
        .route("/api/subsystems", get(list_bug_subsystems))
        .route("/api/bug/logs", get(get_bug_logs))
        .route("/api/bug/raw", get(get_bug_raw))
        .route("/api/bug/input", get(get_bug_input))
        .route("/api/bug/enrichments", get(get_bug_enrichments))
        .route("/api/bug/analyze", post(analyze_bug))
        .route("/api/bug/action", post(bug_action))
        .route("/bug/{bugid}", get(redirect_bug))
        .route("/api/webhook/{provider}", post(forge_webhook))
        .route("/", get_service(ServeFile::new("static/index.html")))
        .nest_service("/static", ServeDir::new("static"))
        .layer(middleware::from_fn(redirect_www))
        .layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024))
        .with_state(state)
}

pub async fn run_server(
    settings: Arc<crate::settings::Settings>,
    db: Arc<Database>,
    sender: mpsc::Sender<Event>,
    fetch_sender: mpsc::Sender<FetchRequest>,
    allow_all_submit: bool,
    smtp_enabled: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = build_router(
        settings.clone(),
        db,
        sender,
        fetch_sender,
        allow_all_submit,
        smtp_enabled,
        dry_run,
    );

    let bind_addr = format!("{}:{}", settings.server.host, settings.server.port);
    let addrs: Vec<SocketAddr> = bind_addr
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("invalid bind address '{}': {}", bind_addr, e))?
        .collect();
    info!("Web API listening on {:?}", addrs);

    let listener = TcpListener::bind(addrs.as_slice()).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

fn generate_synthetic_id(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let start = SystemTime::now();
    let since_the_epoch = start
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards");
    // e.g. sashiko-local-1715890000-12345
    format!(
        "sashiko-{}-{}-{}@sashiko.local",
        prefix,
        since_the_epoch.as_secs(),
        fastrand::u32(..)
    )
}

async fn submit_patch(
    auth: crate::auth::OptionalAuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SubmitRequest>,
) -> Result<Json<SubmitResponse>, StatusCode> {
    if state.read_only {
        return Err(StatusCode::FORBIDDEN);
    }

    if !is_authorized(
        &addr,
        &state,
        &headers,
        auth.0.as_ref(),
        crate::settings::Permission::Ingest,
    ) {
        info!("Refused patch submission from non-localhost: {}", addr);
        return Err(StatusCode::FORBIDDEN);
    }

    match payload {
        SubmitRequest::Inject {
            raw,
            base_commit,
            skip_subjects,
            only_subjects,
        } => {
            if raw.trim().is_empty() {
                return Err(StatusCode::BAD_REQUEST);
            }
            // Basic guardrail
            if !raw.contains("From ") && !raw.contains("Subject:") {
                return Err(StatusCode::BAD_REQUEST);
            }

            let id = generate_synthetic_id("inject");
            info!("Received raw mbox injection: {} (len: {})", id, raw.len());

            let submitted_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .ok();

            let event = Event::RawMboxSubmitted {
                raw,
                submission_id: id.clone(),
                source: MessageSource::ApiInject,
                group: "api-submit".to_string(),
                baseline: base_commit,
                skip_subjects,
                only_subjects,
                submitted_at,
            };

            if let Err(e) = state.sender.send(event).await {
                error!("Failed to send raw mbox to queue: {}", e);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }

            Ok(Json(SubmitResponse {
                status: "accepted".to_string(),
                id,
            }))
        }
        SubmitRequest::Remote {
            sha,
            repo,
            skip_subjects,
            only_subjects,
        }
        | SubmitRequest::RemoteRange {
            sha,
            repo,
            skip_subjects,
            only_subjects,
        } => {
            let id = sha.clone();
            let repo_display = repo.as_deref().unwrap_or("local");
            info!(
                "Received remote fetch request: {} from {}",
                sha, repo_display
            );

            // Optimistic check: If we already have this patchset in the DB,
            // skip creating placeholder and skip fetch queue entirely.
            match state.db.has_patchset_by_msgid(&id).await {
                Ok(true) => {
                    info!(
                        "Remote fetch request for already ingested SHA {}, skipping placeholder and fetch",
                        id
                    );
                    return Ok(Json(SubmitResponse {
                        status: "accepted".to_string(),
                        id,
                    }));
                }
                Err(e) => {
                    error!("Failed to check if patchset exists: {}", e);
                }
                _ => {}
            }

            // Create a placeholder record in the DB so the user can track status
            if let Err(e) = state
                .db
                .create_fetching_patchset(
                    &format!("{}@sashiko.local", id),
                    &format!("Fetching {} from {}...", sha, repo_display),
                    skip_subjects.as_ref(),
                    only_subjects.as_ref(),
                    None,
                    None,
                    None,
                    None,
                )
                .await
            {
                error!("Failed to create placeholder patchset: {}", e);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }

            let req = FetchRequest {
                repo_url: repo,
                commit_hash: sha,
                mr_url: None,
                mr_title: None,
                mr_number: None,
            };

            if let Err(e) = state.fetch_sender.send(req).await {
                error!("Failed to send fetch request to queue: {}", e);
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }

            Ok(Json(SubmitResponse {
                status: "accepted".to_string(),
                id,
            }))
        }
        SubmitRequest::Thread { msgid } => {
            let id = generate_synthetic_id("thread");
            // Percent-encode path-significant characters in the message-ID
            // for safe inclusion in the lore.kernel.org fetch URL. This
            // handles RFC 5322 message-IDs that contain `/` or other
            // path-sensitive characters without rejecting them.
            const PATH_SEGMENT_ENCODE: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
                .add(b'/')
                .add(b'\\')
                .add(b'?')
                .add(b'#')
                .add(b' ')
                .add(b'%');
            let clean_msgid = percent_encoding::utf8_percent_encode(
                msgid.trim_matches(|c| c == '<' || c == '>'),
                PATH_SEGMENT_ENCODE,
            )
            .to_string();
            info!(
                "Received thread fetch request: {} (msgid: {})",
                id, clean_msgid
            );

            // Create a placeholder record in the DB so the user can track status
            if let Err(e) = state
                .db
                .create_fetching_patchset(
                    &clean_msgid,
                    &format!("Fetching thread {}...", clean_msgid),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await
            {
                tracing::error!("Failed to create placeholder patchset: {}", e);
                // Non-fatal, just continue
            }

            let msgid_clone = clean_msgid.clone();
            let sender = state.sender.clone();

            tokio::spawn(async move {
                if let Err(e) = fetch_and_inject_thread(&msgid_clone, sender.clone()).await {
                    tracing::error!("Failed to fetch thread {}: {}", msgid_clone, e);
                    let _ = sender
                        .send(Event::IngestionFailed {
                            article_id: msgid_clone.clone(),
                            error: format!("Failed to fetch thread: {}", e),
                            source: MessageSource::ApiFetchThread,
                        })
                        .await;
                }
            });

            Ok(Json(SubmitResponse {
                status: "accepted".to_string(),
                id, // The client might expect this ID
            }))
        }
    }
}

async fn fetch_and_inject_thread(
    msgid: &str,
    sender: tokio::sync::mpsc::Sender<Event>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("https://lore.kernel.org/all/{}/t.mbox.gz", msgid);
    let response = reqwest::get(&url).await?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to fetch thread {}: HTTP {}",
            msgid,
            response.status()
        )
        .into());
    }

    const MAX_MBOX_DOWNLOAD: usize = 10 * 1024 * 1024;
    const MAX_MBOX_DECOMPRESSED: u64 = 50 * 1024 * 1024;

    let bytes = response.bytes().await?;
    if bytes.len() > MAX_MBOX_DOWNLOAD {
        return Err(format!(
            "Mbox download {} bytes exceeds {} byte limit",
            bytes.len(),
            MAX_MBOX_DOWNLOAD
        )
        .into());
    }

    // Decompress into bytes first, then convert to UTF-8. Reading directly
    // into a String via read_to_string would produce an InvalidData error
    // if the byte limit splits a multi-byte UTF-8 character.
    let raw = tokio::task::spawn_blocking(move || -> Result<String, std::io::Error> {
        use std::io::Read;
        let decoder = flate2::read::GzDecoder::new(&bytes[..]);
        let mut limited = decoder.take(MAX_MBOX_DECOMPRESSED);
        let mut raw_bytes = Vec::new();
        limited.read_to_end(&mut raw_bytes)?;

        // If we read exactly the limit, check whether the stream had more
        // data. This distinguishes a file that is exactly 50 MiB (accept)
        // from one that was truncated at 50 MiB (reject).
        if raw_bytes.len() as u64 == MAX_MBOX_DECOMPRESSED {
            let mut probe = [0u8; 1];
            if limited.into_inner().read(&mut probe).unwrap_or(0) > 0 {
                return Err(std::io::Error::other(format!(
                    "Decompressed mbox exceeds {} byte limit",
                    MAX_MBOX_DECOMPRESSED
                )));
            }
        }
        String::from_utf8(raw_bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    })
    .await??;

    let submitted_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .ok();

    let event = Event::RawMboxSubmitted {
        raw,
        submission_id: msgid.to_string(),
        source: MessageSource::ApiFetchThread,
        group: "api-submit".to_string(),
        baseline: None,
        skip_subjects: None,
        only_subjects: None,
        submitted_at,
    };

    sender.send(event).await?;
    Ok(())
}

async fn list_mailing_lists(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    let lists = state.db.get_mailing_lists().await.map_err(|e| {
        error!("Failed to get mailing lists: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let result = lists
        .into_iter()
        .map(|(name, group)| {
            serde_json::json!({
                "name": name,
                "group": group
            })
        })
        .collect();

    Ok(Json(result))
}

async fn list_patchsets(
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<Pagination>,
) -> Result<Json<PatchsetsResponse>, StatusCode> {
    let page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(50).clamp(1, 100);
    let offset = (page - 1) * per_page;

    let items = if pagination.q.is_none()
        && pagination.mailing_list.is_none()
        && page == 1
        && per_page == 50
    {
        state
            .patchsets_homepage_cache
            .get_or_fetch(|| async {
                state
                    .db
                    .get_patchsets(per_page, offset, None, None)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
            })
            .await?
    } else {
        state
            .db
            .get_patchsets(
                per_page,
                offset,
                pagination.q.clone(),
                pagination.mailing_list.clone(),
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };
    let total = if pagination.q.is_none() && pagination.mailing_list.is_none() {
        state
            .patchsets_count_cache
            .get_or_fetch(|| async {
                state
                    .db
                    .count_patchsets(None, None)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
            })
            .await?
    } else {
        state
            .db
            .count_patchsets(pagination.q.clone(), pagination.mailing_list.clone())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    Ok(Json(PatchsetsResponse {
        items,
        total,
        page,
        per_page,
    }))
}

async fn list_messages(
    State(state): State<Arc<AppState>>,
    Query(pagination): Query<Pagination>,
) -> Result<Json<MessagesResponse>, StatusCode> {
    let page = pagination.page.unwrap_or(1).max(1);
    let per_page = pagination.per_page.unwrap_or(50).clamp(1, 100);
    let offset = (page - 1) * per_page;

    let items = if pagination.q.is_none()
        && pagination.mailing_list.is_none()
        && page == 1
        && per_page == 50
    {
        state
            .messages_homepage_cache
            .get_or_fetch(|| async {
                state
                    .db
                    .get_messages(per_page, offset, None, None)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
            })
            .await?
    } else {
        state
            .db
            .get_messages(
                per_page,
                offset,
                pagination.q.clone(),
                pagination.mailing_list.clone(),
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };
    let total = if pagination.q.is_none() && pagination.mailing_list.is_none() {
        state
            .messages_count_cache
            .get_or_fetch(|| async {
                state
                    .db
                    .count_messages(None, None)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
            })
            .await?
    } else {
        state
            .db
            .count_messages(pagination.q.clone(), pagination.mailing_list.clone())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    };

    Ok(Json(MessagesResponse {
        items,
        total,
        page,
        per_page,
    }))
}

async fn get_patchset(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PatchQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let result = if let Ok(id_val) = query.id.parse::<i64>() {
        info!("Fetching details for patchset id: {}", id_val);
        state
            .db
            .get_patchset_details(id_val, query.page, query.per_page)
            .await
    } else if query.id.contains('-') && !query.id.contains('@') {
        info!("Fetching details for patchset slug: {}", query.id);
        state
            .db
            .get_patchset_details_by_slug(&query.id, query.page, query.per_page)
            .await
    } else {
        info!("Fetching details for patchset msgid: {}", query.id);
        state
            .db
            .get_patchset_details_by_msgid(&query.id, query.page, query.per_page)
            .await
    };

    match result {
        Ok(Some(mut details)) => {
            if let Some(obj) = details.as_object_mut() {
                obj.insert(
                    "smtp_enabled".to_string(),
                    serde_json::Value::Bool(state.smtp_enabled),
                );
                obj.insert(
                    "dry_run".to_string(),
                    serde_json::Value::Bool(state.dry_run),
                );
            }
            Ok(Json(details))
        }
        Ok(None) => {
            info!("Patchset not found: {}", query.id);
            Err(StatusCode::NOT_FOUND)
        }
        Err(e) => {
            info!("Database error: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_review(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ReviewQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let result = if let Some(ps_id) = query.patchset_id {
        info!("Fetching latest review for patchset id: {}", ps_id);
        state.db.get_latest_review_for_patchset(ps_id).await
    } else if let Some(id) = query.id {
        info!("Fetching details for review id: {}", id);
        state.db.get_review_details(id).await
    } else {
        return Err(StatusCode::BAD_REQUEST);
    };

    match result {
        Ok(Some(details)) => Ok(Json(details)),
        Ok(None) => {
            info!("Review not found");
            Err(StatusCode::NOT_FOUND)
        }
        Err(e) => {
            info!("Database error: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_patchset_summary(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PatchQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let result = if let Ok(id_val) = query.id.parse::<i64>() {
        info!("Fetching summary for patchset id: {}", id_val);
        state
            .db
            .get_patchset_summary(id_val, query.page, query.per_page)
            .await
    } else {
        info!("Fetching summary for patchset msgid: {}", query.id);
        state
            .db
            .get_patchset_summary_by_msgid(&query.id, query.page, query.per_page)
            .await
    };

    match result {
        Ok(Some(mut details)) => {
            if let Some(obj) = details.as_object_mut() {
                obj.insert(
                    "smtp_enabled".to_string(),
                    serde_json::Value::Bool(state.smtp_enabled),
                );
                obj.insert(
                    "dry_run".to_string(),
                    serde_json::Value::Bool(state.dry_run),
                );
            }
            Ok(Json(details))
        }
        Ok(None) => {
            info!("Patchset not found: {}", query.id);
            Err(StatusCode::NOT_FOUND)
        }
        Err(e) => {
            info!("Database error: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_review_log(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ReviewQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let result = if let Some(ps_id) = query.patchset_id {
        info!("Fetching latest review log for patchset id: {}", ps_id);
        state.db.get_latest_review_for_patchset(ps_id).await
    } else if let Some(id) = query.id {
        info!("Fetching details for review id: {}", id);
        state.db.get_review_details(id).await
    } else {
        return Err(StatusCode::BAD_REQUEST);
    };

    match result {
        Ok(Some(details)) => Ok(Json(details)),
        Ok(None) => {
            info!("Review not found");
            Err(StatusCode::NOT_FOUND)
        }
        Err(e) => {
            info!("Database error: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_bug(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BugQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let result = if let Some(id) = query.id {
        state.db.get_bug(id).await
    } else if let Some(bugid) = query.bugid.as_ref().or(query.slug.as_ref()) {
        state.db.get_bug_by_bugid(bugid).await
    } else {
        return Err(StatusCode::BAD_REQUEST);
    };

    match result {
        Ok(Some(bug)) => {
            let mut val = serde_json::to_value(&bug).unwrap_or(serde_json::json!({}));
            val["slug"] = serde_json::Value::String(bug.bugid.clone());
            val["problem"] = serde_json::Value::String(bug.problem().to_string());
            val["severity"] = serde_json::to_value(bug.severity()).unwrap();
            val["severity_explanation"] = serde_json::to_value(bug.severity_explanation()).unwrap();
            val["description"] = serde_json::to_value(bug.description()).unwrap();
            val["inline_review"] = serde_json::Value::String(bug.inline_review());
            val["locations"] = serde_json::to_value(bug.locations()).unwrap();
            val["source_files"] = serde_json::to_value(bug.source_files()).unwrap();
            val["introduced_in_commit"] = serde_json::to_value(bug.introduced_in_commit()).unwrap();
            val["verified_on_sha"] = serde_json::to_value(bug.verified_on_sha()).unwrap();
            val["is_fixed"] = serde_json::Value::Bool(bug.is_fixed());
            val["fixed_in_commit"] = serde_json::to_value(bug.fixed_in_commit()).unwrap();
            val["raw_input"] = serde_json::to_value(bug.raw_input()).unwrap();
            val["tokens_in"] = serde_json::Value::Number(bug.tokens_in().into());
            val["tokens_out"] = serde_json::Value::Number(bug.tokens_out().into());
            val["tokens_cached"] = serde_json::Value::Number(bug.tokens_cached().into());

            if bug.lifecycle_status == crate::db::BugLifecycleStatus::Duplicate {
                let canonical = if let Some(canon_id) = bug.duplicate_of_id {
                    state.db.get_bug(canon_id).await.ok().flatten()
                } else {
                    None
                };
                if let Some(c) = canonical {
                    val["duplicate_of"] = serde_json::json!({
                        "id": c.id,
                        "bugid": c.bugid,
                        "problem": c.problem(),
                    });
                }
            } else if let Ok(dups) = state.db.list_duplicates_for_bug(bug.id).await {
                let dup_summaries: Vec<serde_json::Value> = dups
                    .into_iter()
                    .map(|d| {
                        serde_json::json!({
                            "id": d.id,
                            "bugid": d.bugid,
                            "problem": d.problem(),
                            "created_at": d.created_at,
                        })
                    })
                    .collect();
                val["duplicates"] = serde_json::Value::Array(dup_summaries);
            }
            val["evidence"] = state
                .db
                .bug_evidence(bug.id)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if let Some(obj) = val.as_object_mut() {
                for key in [
                    "raw_input",
                    "vector_json",
                    "enrichments",
                    "tokens_in",
                    "tokens_out",
                    "tokens_cached",
                ] {
                    obj.remove(key);
                }
            }
            Ok(Json(val))
        }
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!("Database error fetching preexisting bug: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_bug_raw(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BugQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bug = if let Some(id) = query.id {
        state.db.get_bug(id).await
    } else if let Some(bugid) = query.bugid.as_ref().or(query.slug.as_ref()) {
        state.db.get_bug_by_bugid(bugid).await
    } else {
        return Err(StatusCode::BAD_REQUEST);
    }
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let records = state
        .db
        .bug_family(bug.id, true)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        serde_json::json!({ "bugid": bug.bugid, "records": records }),
    ))
}

/// Serves the payload the bug workflow was started with for a single bug.
/// Duplicates keep their own candidate records, so each one resolves to the
/// input that produced it rather than the canonical bug's input.
async fn get_bug_input(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BugQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bug = if let Some(id) = query.id {
        state.db.get_bug(id).await
    } else if let Some(bugid) = query.bugid.as_ref().or(query.slug.as_ref()) {
        state.db.get_bug_by_bugid(bugid).await
    } else {
        return Err(StatusCode::BAD_REQUEST);
    }
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let inputs: Vec<serde_json::Value> = bug
        .enrichments
        .iter()
        .filter(|e| e.kind == "candidate" || e.kind == "raw_candidate")
        .map(|e| {
            serde_json::json!({
                "enrichment_id": e.id,
                "created_at": e.created_at,
                "author": e.author,
                "tool": e.tool,
                "model": e.model,
                "input": e.data_json,
                "content": e.content,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "bugid": bug.bugid,
        "title": bug.title,
        "inputs": inputs,
    })))
}

async fn get_bug_enrichments(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BugQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bug = if let Some(id) = query.id {
        state.db.get_bug(id).await
    } else if let Some(bugid) = query.bugid.as_ref().or(query.slug.as_ref()) {
        state.db.get_bug_by_bugid(bugid).await
    } else {
        return Err(StatusCode::BAD_REQUEST);
    };

    match bug {
        Ok(Some(bug)) => Ok(Json(
            serde_json::to_value(&bug.enrichments).unwrap_or_default(),
        )),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!("Database error fetching enrichments: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_bug_logs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BugQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let result = if let Some(id) = query.id {
        state.db.get_bug_logs(id).await
    } else if let Some(bugid) = query.bugid.as_ref().or(query.slug.as_ref()) {
        state.db.get_bug_logs_by_bugid(bugid).await
    } else {
        return Err(StatusCode::BAD_REQUEST);
    };

    match result {
        Ok(Some(logs_str)) => {
            let parsed: serde_json::Value =
                serde_json::from_str(&logs_str).unwrap_or(serde_json::Value::String(logs_str));
            Ok(Json(parsed))
        }
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::error!("Database error fetching bug logs: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

#[derive(serde::Deserialize)]
struct AnalyzeBugPayload {
    #[serde(flatten)]
    input: crate::workflows::linux_bug::BugInput,
    tool: Option<String>,
    model: Option<String>,
}

async fn analyze_bug(
    auth: crate::auth::OptionalAuthUser,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AnalyzeBugPayload>,
) -> Result<Json<crate::workflows::linux_bug::BugOutcome>, (StatusCode, String)> {
    if state.read_only {
        return Err((
            StatusCode::FORBIDDEN,
            "Server is running in read-only mode.".to_string(),
        ));
    }

    if !is_authorized(
        &addr,
        &state,
        &headers,
        auth.0.as_ref(),
        crate::settings::Permission::Review,
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            "You don't have permissions to analyze bugs.".to_string(),
        ));
    }

    let provider = match crate::ai::create_provider_cached(&state.settings, false, 0).await {
        Ok(p) => p,
        Err(e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to create AI provider: {}", e),
            ));
        }
    };

    let repo_path = std::path::PathBuf::from(&state.settings.git.repository_path);
    let tools = if repo_path.exists() {
        let mainline_sha = match crate::git_ops::get_commit_hash(&repo_path, "origin/master").await
        {
            Ok(sha) => Some(sha),
            Err(_) => match crate::git_ops::get_commit_hash(&repo_path, "master").await {
                Ok(sha) => Some(sha),
                Err(_) => crate::git_ops::get_commit_hash(&repo_path, "HEAD")
                    .await
                    .ok(),
            },
        };
        let mut tb = crate::toolbox::ToolBox::new(repo_path, None);
        if let Some(m_sha) = mainline_sha {
            tb.set_virtual_head(m_sha);
        }
        Some(std::sync::Arc::new(tb))
    } else {
        None
    };

    let source_tool = payload
        .tool
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "api".into());
    let source_model = payload
        .model
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some(provider.get_capabilities().model_name));
    let mut payload = payload.input;
    if payload.subsystems.is_empty() && !payload.source_files.is_empty() {
        if let Some(mindex) = crate::maintainers::get_global_maintainers() {
            payload.subsystems = mindex.match_files(&payload.source_files);
        } else if let Ok(mindex) = crate::maintainers::MaintainersIndex::from_top_of_trunk(
            &state.settings.git.repository_path,
        ) {
            payload.subsystems = mindex.match_files(&payload.source_files);
        }
    }

    let actor = auth
        .0
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_else(|| format!("authorized client ({})", addr.ip()));
    let attributed_db = state.db.with_bug_actor(&actor, &source_tool, source_model);
    match crate::workflows::linux_bug::process_issue(
        provider.as_ref(),
        tools,
        &attributed_db,
        payload,
        Some("api_analyze"),
    )
    .await
    {
        Ok(outcome) => Ok(Json(outcome)),
        Err(e) => {
            tracing::error!("Pre-existing bug analysis failed: {}", e);
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Analysis failed: {}", e),
            ))
        }
    }
}

async fn redirect_bug(Path(bugid): Path<String>) -> impl IntoResponse {
    Redirect::temporary(&format!("/#/bug/{}", bugid))
}

async fn get_message(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PatchQuery>,
) -> Result<Json<crate::db::MessageRow>, StatusCode> {
    let result = if let Ok(id_val) = query.id.parse::<i64>() {
        info!("Fetching details for message id: {}", id_val);
        state.db.get_message_details(id_val).await
    } else {
        info!("Fetching details for message msgid: {}", query.id);
        state.db.get_message_details_by_msgid(&query.id).await
    };

    match result {
        Ok(Some(mut details)) => {
            if (details.body.is_none() || details.body.as_deref() == Some(""))
                && let (Some(hash), Some(group)) = (&details.git_blob_hash, &details.mailing_list)
            {
                let repo_root = std::path::PathBuf::from("archives").join(group);

                // 1. Find all potential repo paths (root + epochs)
                let mut candidate_paths = Vec::new();

                // Check epochs first (most likely for recent messages)
                if let Ok(mut entries) = tokio::fs::read_dir(&repo_root).await {
                    let mut epochs = Vec::new();
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        if let Ok(ft) = entry.file_type().await
                            && ft.is_dir()
                            && let Ok(name) = entry.file_name().into_string()
                            && let Ok(num) = name.parse::<i32>()
                        {
                            epochs.push(num);
                        }
                    }
                    epochs.sort_by(|a, b| b.cmp(a)); // Descending

                    for epoch in epochs {
                        candidate_paths.push(repo_root.join(epoch.to_string()));
                    }
                }

                // Add root as fallback
                candidate_paths.push(repo_root.clone());

                // 2. Search for blob
                for path in candidate_paths {
                    if let Ok(raw) = crate::git_ops::read_blob(&path, hash).await
                        && let Ok((metadata, _)) = crate::patch::parse_email(&raw)
                    {
                        details.body = Some(metadata.body);
                        break;
                    }
                }
            }
            Ok(Json(details))
        }
        Ok(None) => {
            info!("Message not found: {}", query.id);
            Err(StatusCode::NOT_FOUND)
        }
        Err(e) => {
            info!("Database error: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn get_stats(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let pending = crate::metrics::get_pending_patches();
    let reviewing = crate::metrics::get_reviewing_patches();
    let messages = crate::metrics::get_messages();
    let patchsets = crate::metrics::get_patchsets();
    let repo_packs = crate::metrics::get_repo_packs();
    let repo_pack_bytes = crate::metrics::get_repo_pack_bytes();

    Ok(Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "pending": pending,
        "reviewing": reviewing,
        "messages": messages,
        "patchsets": patchsets,
        "repo_packs": repo_packs,
        "repo_pack_bytes": repo_pack_bytes
    })))
}

async fn stats_timeline(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SubsystemQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let data = state
        .stats_timeline_cache
        .get_or_fetch(params.subsystem_id, || async {
            state
                .db
                .get_timeline_stats(params.subsystem_id)
                .await
                .map_err(|e| {
                    info!("Error getting timeline stats: {}", e);
                    StatusCode::INTERNAL_SERVER_ERROR
                })
        })
        .await?;
    Ok(Json(data))
}

async fn stats_reviews(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let data = state
        .stats_reviews_cache
        .get_or_fetch(|| async {
            state.db.get_review_stats().await.map_err(|e| {
                info!("Error getting review stats: {}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })
        })
        .await?;
    Ok(Json(data))
}

async fn stats_tools(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let data = state
        .stats_tools_cache
        .get_or_fetch(|| async {
            state.db.get_tool_usage_stats().await.map_err(|e| {
                info!("Error getting tool stats: {}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })
        })
        .await?;
    Ok(Json(data))
}

async fn rerun_patchset(
    auth: crate::auth::OptionalAuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Query(query): Query<PatchQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.read_only {
        return Err((
            StatusCode::FORBIDDEN,
            "Server is running in read-only mode.".into(),
        ));
    }

    if !is_authorized(
        &addr,
        &state,
        &headers,
        auth.0.as_ref(),
        crate::settings::Permission::Review,
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            "You don't have permissions to rerun patchsets.".into(),
        ));
    }

    let id = query
        .id
        .parse::<i64>()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid id parameter".into()))?;

    state.db.rerun_patchset(id).await.map_err(|e| {
        error!("Failed to rerun patchset {}: {}", id, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to rerun patchset".into(),
        )
    })?;

    Ok(Json(serde_json::json!({ "status": "accepted" })))
}

async fn cancel_patchset(
    auth: crate::auth::OptionalAuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Query(query): Query<CancelQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.read_only {
        return Err((
            StatusCode::FORBIDDEN,
            "Server is running in read-only mode.".into(),
        ));
    }

    if !is_authorized(
        &addr,
        &state,
        &headers,
        auth.0.as_ref(),
        crate::settings::Permission::Cancel,
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            "You don't have permissions to cancel patchsets.".into(),
        ));
    }

    let cancelled = state
        .db
        .cancel_patchset(query.id, query.force)
        .await
        .map_err(|e| {
            error!("Failed to cancel patchset {}: {}", query.id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to cancel patchset".into(),
            )
        })?;

    if cancelled {
        info!("Patchset {} cancelled (force={})", query.id, query.force);
        Ok(Json(serde_json::json!({ "status": "cancelled" })))
    } else {
        let reason = if query.force {
            "Patchset is not in a cancellable state (must be Pending, Incomplete, or In Review)"
        } else {
            "Patchset is not in a cancellable state (must be Pending or Incomplete; use force=true for In Review)"
        };
        Ok(Json(serde_json::json!({
            "status": "not_modified",
            "reason": reason
        })))
    }
}

async fn rerun_patch(
    auth: crate::auth::OptionalAuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Query(query): Query<RerunPatchQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.read_only {
        return Err((
            StatusCode::FORBIDDEN,
            "Server is running in read-only mode.".into(),
        ));
    }

    if !is_authorized(
        &addr,
        &state,
        &headers,
        auth.0.as_ref(),
        crate::settings::Permission::Review,
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            "You don't have permissions to rerun patches.".into(),
        ));
    }

    state
        .db
        .rerun_patch(query.patchset_id, query.patch_id)
        .await
        .map_err(|e| {
            error!(
                "Failed to rerun patch {} in patchset {}: {}",
                query.patch_id, query.patchset_id, e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to rerun patch".into(),
            )
        })?;

    Ok(Json(serde_json::json!({ "status": "accepted" })))
}

async fn health_check() -> StatusCode {
    StatusCode::OK
}

async fn get_config(
    auth: crate::auth::OptionalAuthUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let is_auth = |perm| is_authorized(&addr, &state, &headers, auth.0.as_ref(), perm);
    let can_action = !state.read_only && is_auth(crate::settings::Permission::Action);
    let can_review = !state.read_only && is_auth(crate::settings::Permission::Review);
    let can_cancel = !state.read_only && is_auth(crate::settings::Permission::Cancel);
    let can_ingest = !state.read_only && is_auth(crate::settings::Permission::Ingest);

    Ok(Json(serde_json::json!({
        "project_name": state.settings.project.name,
        "project_description": state.settings.project.description,
        "project_domain": state.settings.project.domain,
        "attribution": state.settings.project.attribution(),
        "forge_enabled": state.settings.forge.enabled,
        "read_only": state.read_only,
        "permissions": {
            "action": can_action,
            "review": can_review,
            "cancel": can_cancel,
            "ingest": can_ingest,
        },
        "user": {
            "email": auth.0.as_ref().map(|u| &u.email),
            "is_authenticated": auth.0.is_some(),
        },
        "version": env!("CARGO_PKG_VERSION"),
        "git_hash": env!("GIT_HASH"),
    })))
}

async fn forge_webhook(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.read_only {
        return Err(StatusCode::FORBIDDEN);
    }

    let webhook_secret = state.settings.forge.webhook_secret.as_deref();
    let has_secret = webhook_secret.is_some();

    // Access control: when webhook_secret is configured, signature
    // verification in validate_event is the sole access control for ALL
    // requests. This is critical for reverse proxy deployments where all
    // traffic arrives from loopback — the signature check cannot be
    // bypassed by source IP.
    //
    // When no secret is configured, fall back to localhost-only or the
    // explicit --enable-unsafe-all-submit flag. Note: this is insecure
    // behind a reverse proxy; operators MUST configure webhook_secret
    // for proxied deployments.
    if !has_secret {
        let is_loopback = addr.ip().to_canonical().is_loopback();
        if !is_loopback && !state.allow_all_submit {
            info!(
                "Refused {} webhook from {}: configure webhook_secret or use --enable-unsafe-all-submit",
                provider, addr
            );
            return Err(StatusCode::FORBIDDEN);
        }
    }

    let forge = state.forge_registry.get(&provider).ok_or_else(|| {
        warn!("Unknown forge provider: {}", provider);
        StatusCode::NOT_FOUND
    })?;

    forge.validate_event(&headers, &body, webhook_secret)?;

    let (action, metadata) = forge.parse_payload(&body)?;

    info!(
        "{} {}: {} - {}",
        forge.name(),
        action,
        metadata.pr_title.as_deref().unwrap_or("(no title)"),
        metadata.pr_url.as_deref().unwrap_or("(no url)")
    );

    let default_subject = format!("{} #{}", forge.name(), metadata.pr_number);
    let subject = metadata.pr_title.as_deref().unwrap_or(&default_subject);

    let commit_range = format!("{}..{}", metadata.base_sha, metadata.head_sha);
    let placeholder_id = format!("mr-{}-{}", metadata.pr_number, commit_range);

    let slug = metadata.repo_url.as_ref().map(|url| {
        let repo = crate::forge::extract_repo_name_from_url(url);
        format!("{}-{}", repo, metadata.pr_number)
    });

    state
        .db
        .create_fetching_patchset(
            &placeholder_id,
            &format!("Fetching {} PR/MR: {}", forge.name(), subject),
            None,
            None,
            metadata.pr_url.as_deref(),
            Some(subject),
            Some(metadata.pr_number),
            slug.as_deref(),
        )
        .await
        .map_err(|e| {
            error!("Failed to create placeholder patchset: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let req = FetchRequest {
        repo_url: metadata.repo_url,
        commit_hash: commit_range,
        mr_url: metadata.pr_url,
        mr_title: metadata.pr_title,
        mr_number: Some(metadata.pr_number),
    };

    state.fetch_sender.send(req).await.map_err(|e| {
        error!("Failed to send fetch request to queue: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(serde_json::json!({
        "status": "accepted",
        "message": format!("{} {} queued for review", forge.name(), action)
    })))
}

async fn list_bugs(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BugListQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(50).clamp(1, 100);

    let min_sev = query
        .min_severity
        .as_deref()
        .or(query.severity.as_deref())
        .map(crate::db::Severity::from_str);

    let parsed_subsystems: Option<Vec<String>> = if let Some(ref subs_str) = query.subsystems {
        let subs: Vec<String> = if subs_str.trim().starts_with('[') {
            serde_json::from_str(subs_str).unwrap_or_else(|_| {
                subs_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
        } else {
            subs_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        Some(subs)
    } else if let Some(ref sub_str) = query.subsystem
        && sub_str.contains(',')
    {
        let subs: Vec<String> = sub_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Some(subs)
    } else {
        None
    };

    // Unknown filter values are rejected outright: silently returning an empty
    // list would look identical to "no bugs match" and hide the typo.
    let lifecycle_status = match query.lifecycle_status.as_deref().map(str::parse) {
        Some(Ok(status)) => Some(status),
        Some(Err(_)) => return Err(StatusCode::BAD_REQUEST),
        None => None,
    };
    let pipeline_state = match query.pipeline_state.as_deref().map(str::parse) {
        Some(Ok(state)) => Some(state),
        Some(Err(_)) => return Err(StatusCode::BAD_REQUEST),
        None => None,
    };
    let assignee = query
        .assignee
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|who| {
            if who.eq_ignore_ascii_case("none") {
                crate::db::AssigneeFilter::Unassigned
            } else {
                crate::db::AssigneeFilter::Is(who)
            }
        });

    match state
        .db
        .list_bugs(crate::db::ListBugsParams {
            page: Some(page as u32),
            limit: Some(per_page as u32),
            min_severity: min_sev,
            subsystem: if parsed_subsystems.is_none() {
                query.subsystem.as_deref()
            } else {
                None
            },
            subsystems: parsed_subsystems.as_deref(),
            lifecycle_status,
            pipeline_state,
            assignee,
            search: query.q.as_deref(),
            sort_by: query.sort_by.as_deref(),
            sort_order: query.sort_order.as_deref(),
        })
        .await
    {
        Ok((items, total)) => {
            let ids: Vec<_> = items.iter().map(|b| b.id).collect();
            let mut summaries = state
                .db
                .bug_discovery_summaries(&ids)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let mut serialized_items = Vec::new();
            for bug in &items {
                let mut val = serde_json::to_value(bug).unwrap_or(serde_json::json!({}));
                val["slug"] = serde_json::Value::String(bug.bugid.clone());
                val["problem"] = serde_json::Value::String(bug.problem().to_string());
                val["severity"] = serde_json::to_value(bug.severity()).unwrap();
                val["severity_explanation"] =
                    serde_json::to_value(bug.severity_explanation()).unwrap();
                val["description"] = serde_json::to_value(bug.description()).unwrap();
                val["inline_review"] = serde_json::Value::String(bug.inline_review());
                val["locations"] = serde_json::to_value(bug.locations()).unwrap();
                val["source_files"] = serde_json::to_value(bug.source_files()).unwrap();
                val["introduced_in_commit"] =
                    serde_json::to_value(bug.introduced_in_commit()).unwrap();
                val["verified_on_sha"] = serde_json::to_value(bug.verified_on_sha()).unwrap();
                val["is_fixed"] = serde_json::Value::Bool(bug.is_fixed());
                val["fixed_in_commit"] = serde_json::to_value(bug.fixed_in_commit()).unwrap();
                val["raw_input"] = serde_json::to_value(bug.raw_input()).unwrap();
                val["tokens_in"] = serde_json::Value::Number(bug.tokens_in().into());
                val["tokens_out"] = serde_json::Value::Number(bug.tokens_out().into());
                val["tokens_cached"] = serde_json::Value::Number(bug.tokens_cached().into());
                val.as_object_mut().map(|obj| obj.remove("enrichments"));
                val["evidence"] = summaries.remove(&bug.id).unwrap_or_else(|| serde_json::json!({"count": 0, "models": [], "tools": [], "unknown_models": 0}));
                val.as_object_mut().unwrap().remove("raw_input");
                val.as_object_mut().unwrap().remove("vector_json");
                serialized_items.push(val);
            }
            Ok(Json(serde_json::json!({
                "items": serialized_items,
                "total": total,
                "page": page,
                "per_page": per_page
            })))
        }
        Err(e) => {
            tracing::error!("Failed to fetch bugs list: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn list_bug_subsystems(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BugSubsystemsQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let lifecycle_status = match query.lifecycle_status.as_deref().map(str::parse) {
        Some(Ok(status)) => status,
        Some(Err(_)) => return Err(StatusCode::BAD_REQUEST),
        None => crate::db::BugLifecycleStatus::Open,
    };
    // Key the cache on the parsed value so the alias and the canonical spelling
    // do not each get their own entry.
    let status_key = Some(lifecycle_status.as_str().to_string());

    let res = state
        .bug_subsystems_cache
        .get_or_fetch(status_key, || async {
            let db = state.db.clone();
            let counts = db
                .get_subsystems_bug_counts(Some(lifecycle_status))
                .await
                .map_err(|e| {
                    tracing::error!("Failed to fetch bug subsystem counts: {}", e);
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;

            let list: Vec<serde_json::Value> = counts
                .into_iter()
                .map(|(name, count)| {
                    serde_json::json!({
                        "name": name,
                        "count": count,
                        "open_bugs": count,
                    })
                })
                .collect();
            Ok::<Vec<serde_json::Value>, StatusCode>(list)
        })
        .await?;

    Ok(Json(serde_json::Value::Array(res)))
}

#[derive(Debug, serde::Deserialize)]
pub struct BugActionPayload {
    /// Client-declared provenance. Author always comes from authentication.
    pub tool: Option<String>,
    pub model: Option<String>,
    #[serde(flatten)]
    pub action: BugAction,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BugAction {
    Comment {
        content: String,
    },
    Close {
        reason: Option<String>,
    },
    Dismiss {
        reason: Option<String>,
    },
    MarkDuplicate {
        duplicate_of_id: Option<i64>,
        duplicate_of_bugid: Option<String>,
        reasoning: Option<String>,
    },
}

async fn bug_action(
    auth: crate::auth::OptionalAuthUser,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<BugQuery>,
    axum::extract::Json(payload): axum::extract::Json<BugActionPayload>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if state.read_only {
        return Err((
            StatusCode::FORBIDDEN,
            "Server is running in read-only mode.".into(),
        ));
    }

    if !is_authorized(
        &addr,
        &state,
        &headers,
        auth.0.as_ref(),
        crate::settings::Permission::Action,
    ) {
        return Err((
            StatusCode::FORBIDDEN,
            "You don't have permissions to perform bug actions.".into(),
        ));
    }

    let bug_res = if let Some(id) = query.id {
        state.db.get_bug(id).await
    } else if let Some(bugid) = query.bugid.as_ref().or(query.slug.as_ref()) {
        state.db.get_bug_by_bugid(bugid).await
    } else {
        return Err((StatusCode::BAD_REQUEST, "Missing id or bugid param".into()));
    };

    let bug = bug_res.map_err(|e| {
        tracing::error!("Database error fetching bug: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "Database error".into())
    })?;

    let bug = match bug {
        Some(b) => b,
        None => return Err((StatusCode::NOT_FOUND, "Bug not found".into())),
    };

    let actor = auth
        .0
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_else(|| format!("authorized client ({})", addr.ip()));
    let tool = payload
        .tool
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("api");
    let model = payload.model.filter(|s| !s.trim().is_empty());
    let db = state.db.with_bug_actor(&actor, tool, model);
    match payload.action {
        BugAction::Comment { content } => {
            if content.trim().is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "Comment content is required".into(),
                ));
            }
            db.add_bug_enrichment(
                bug.id,
                &crate::db::NewBugEnrichment {
                    kind: "comment".into(),
                    content: Some(content),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        BugAction::Close { reason } => {
            db.change_bug_status_with_reason(
                bug.id,
                crate::db::BugLifecycleStatus::Closed,
                reason.as_deref(),
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        BugAction::Dismiss { reason } => {
            db.change_bug_status_with_reason(
                bug.id,
                crate::db::BugLifecycleStatus::Dismissed,
                reason.as_deref(),
            )
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        BugAction::MarkDuplicate {
            duplicate_of_id,
            duplicate_of_bugid,
            reasoning,
        } => {
            let target = match (duplicate_of_id, duplicate_of_bugid) {
                (Some(id), None) => db.get_bug(id).await,
                (None, Some(bugid)) => db.get_bug_by_bugid(bugid.trim()).await,
                _ => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "Provide one existing bug ID".into(),
                    ));
                }
            }
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let target = target
                .filter(|b| b.id != bug.id && b.duplicate_of_id.is_none())
                .ok_or_else(|| {
                    (
                        StatusCode::BAD_REQUEST,
                        "Choose an existing canonical bug, distinct from this bug".into(),
                    )
                })?;
            let duplicate_of_id = target.id;
            db.mark_bug_as_duplicate(crate::db::MarkDuplicateBugParams {
                ephemeral_id: bug.id,
                canonical_id: duplicate_of_id,
                reasoning: reasoning.as_deref().unwrap_or("Marked as duplicate"),
                ..Default::default()
            })
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }

    Ok(Json(serde_json::json!({ "status": "success" })))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_synthetic_id_format() {
        let id = generate_synthetic_id("test");
        assert!(id.starts_with("sashiko-test-"));
        assert!(id.ends_with("@sashiko.local"));
    }

    #[tokio::test]
    async fn test_bug_input_endpoint_serves_per_bug_payload() {
        let db_settings = crate::settings::DatabaseSettings {
            url: ":memory:".to_string(),
            token: String::new(),
        };
        let db = Arc::new(Database::new(&db_settings).await.unwrap());
        db.migrate().await.unwrap();

        let make_bug = |bugid: &str, title: &str| crate::db::NewBug {
            bugid: bugid.to_string(),
            title: title.to_string(),
            lifecycle_status: crate::db::BugLifecycleStatus::New,
            pipeline_state: crate::db::BugPipelineState::Pending,
            assignee: None,
            reporter: "sashiko".to_string(),
            reported_at: 100,
            discovered_in_patchset_id: None,
            discovered_in_patch_id: None,
            discovered_in_commit: None,
            source_ref: None,
            vector_json: None,
            duplicate_of_id: None,
            subsystems: vec![],
        };

        let canonical = db
            .create_bug_with_enrichment(
                &make_bug("linux-canonical", "UAF in canonical path"),
                Some(&crate::db::NewBugEnrichment {
                    kind: "candidate".to_string(),
                    tool: "sashiko:linux_patch_review".to_string(),
                    model: Some("test-model".to_string()),
                    created_at: 100,
                    content: Some("canonical reasoning".to_string()),
                    data_json: Some(serde_json::json!({"problem": "canonical problem"})),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        let duplicate = db
            .create_bug_with_enrichment(
                &make_bug("linux-duplicate", "UAF spotted again"),
                Some(&crate::db::NewBugEnrichment {
                    kind: "candidate".to_string(),
                    tool: "sashiko:linux_patch_review".to_string(),
                    model: Some("test-model".to_string()),
                    created_at: 200,
                    content: Some("duplicate reasoning".to_string()),
                    data_json: Some(serde_json::json!({"problem": "duplicate problem"})),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        db.mark_bug_as_duplicate(crate::db::MarkDuplicateBugParams {
            ephemeral_id: duplicate,
            canonical_id: canonical,
            reasoning: "same defect",
            logs: None,
            tokens_in: None,
            tokens_out: None,
            tokens_cached: None,
        })
        .await
        .unwrap();

        let settings = Arc::new(crate::settings::Settings::new().unwrap());
        let (event_tx, _event_rx) = mpsc::channel(10);
        let (fetch_tx, _fetch_rx) = mpsc::channel(10);
        let app = build_router(
            settings,
            db.clone(),
            event_tx,
            fetch_tx,
            false,
            false,
            false,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let canonical_input: serde_json::Value = reqwest::get(format!(
            "http://{}/api/bug/input?bugid=linux-canonical",
            addr
        ))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(canonical_input["bugid"], "linux-canonical");
        let inputs = canonical_input["inputs"].as_array().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["input"]["problem"], "canonical problem");

        // A duplicate resolves to the payload that produced it, not to the
        // canonical bug's payload.
        let duplicate_input: serde_json::Value =
            reqwest::get(format!("http://{}/api/bug/input?id={}", addr, duplicate))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        let inputs = duplicate_input["inputs"].as_array().unwrap();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0]["input"]["problem"], "duplicate problem");
        assert_eq!(inputs[0]["model"], "test-model");

        let missing = reqwest::get(format!("http://{}/api/bug/input?id=999999", addr))
            .await
            .unwrap();
        assert_eq!(missing.status(), 404);
    }

    #[tokio::test]
    async fn test_bug_endpoints() {
        let db_settings = crate::settings::DatabaseSettings {
            url: ":memory:".to_string(),
            token: String::new(),
        };
        let db = Arc::new(Database::new(&db_settings).await.unwrap());
        db.migrate().await.unwrap();

        let bug_id = db
            .create_bug(&crate::db::NewBug {
                bugid: "linux-12345678".to_string(),
                title: "UAF in test_device".to_string(),
                lifecycle_status: crate::db::BugLifecycleStatus::New,
                pipeline_state: crate::db::BugPipelineState::Pending,
                assignee: None,
                reporter: "sashiko".to_string(),
                reported_at: 123456,
                discovered_in_patchset_id: None,
                discovered_in_patch_id: None,
                discovered_in_commit: None,
                source_ref: None,
                vector_json: None,
                duplicate_of_id: None,
                subsystems: vec!["drivers/net".to_string()],
            })
            .await
            .unwrap();

        db.add_bug_enrichment(
            bug_id,
            &crate::db::NewBugEnrichment {
                kind: "severity_calibration".to_string(),
                tool: "sashiko".to_string(),
                model: None,
                author: None,
                created_at: 123456,
                content: Some("Trace".to_string()),
                data_json: Some(serde_json::json!({
                    "severity": "Critical",
                    "severity_int": 4,
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        db.add_bug_enrichment(
            bug_id,
            &crate::db::NewBugEnrichment {
                kind: "report".to_string(),
                tool: "sashiko".to_string(),
                model: None,
                author: None,
                created_at: 123457,
                content: Some("Inline review".to_string()),
                data_json: None,
                tokens_in: None,
                tokens_out: None,
                tokens_cached: None,
                logs: Some("[{\"role\":\"user\",\"content\":\"test\"}]".to_string()),
            },
        )
        .await
        .unwrap();

        let mut settings = crate::settings::Settings::new().unwrap();
        settings.server.jwt_secret = Some("bug-test-secret-12345678901234567890".into());
        let settings = Arc::new(settings);
        let (event_tx, _event_rx) = mpsc::channel(10);
        let (fetch_tx, _fetch_rx) = mpsc::channel(10);

        let app = build_router(
            settings.clone(),
            db.clone(),
            event_tx,
            fetch_tx,
            false,
            false,
            true,
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        // Test 1: get_bug by bugid (logs omitted)
        let res = reqwest::get(format!("http://{}/api/bug?bugid=linux-12345678", addr))
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let json: serde_json::Value = res.json().await.unwrap();
        assert_eq!(json["bugid"], "linux-12345678");
        assert_eq!(json["problem"], "UAF in test_device");
        assert!(json["logs"].is_null());
        assert!(json.get("enrichments").is_none());
        assert!(json.get("raw_input").is_none());
        assert_eq!(json["evidence"]["count"], 1);
        assert_eq!(json["evidence"]["activity"].as_array().unwrap().len(), 4);

        // Test 1c: get_bug_enrichments
        let res_enrich = reqwest::get(format!(
            "http://{}/api/bug/enrichments?bugid=linux-12345678",
            addr
        ))
        .await
        .unwrap();
        assert_eq!(res_enrich.status(), 200);
        let enrich_json: serde_json::Value = res_enrich.json().await.unwrap();
        assert_eq!(enrich_json.as_array().unwrap().len(), 4);
        assert!(
            enrich_json
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["kind"] == "report")
        );

        // Test 1b: get_bug by legacy slug param
        let res_slug = reqwest::get(format!("http://{}/api/bug?slug=linux-12345678", addr))
            .await
            .unwrap();
        assert_eq!(res_slug.status(), 200);

        // Test 2: get_bug_logs by bugid
        let res_logs = reqwest::get(format!("http://{}/api/bug/logs?bugid=linux-12345678", addr))
            .await
            .unwrap();
        assert_eq!(res_logs.status(), 200);
        let logs_json: serde_json::Value = res_logs.json().await.unwrap();
        assert!(logs_json.is_array());
        assert_eq!(logs_json[0]["role"], "user");

        // Test 2b: get_bug_logs by legacy slug param
        let res_logs_slug =
            reqwest::get(format!("http://{}/api/bug/logs?slug=linux-12345678", addr))
                .await
                .unwrap();
        assert_eq!(res_logs_slug.status(), 200);

        // Test 2b: get_bug_logs by id
        let res_logs_id = reqwest::get(format!("http://{}/api/bug/logs?id={}", addr, bug_id))
            .await
            .unwrap();
        assert_eq!(res_logs_id.status(), 200);

        // Test 3: list_bugs with subsystem filter
        let res_sub = reqwest::get(format!("http://{}/api/bugs?subsystem=drivers/net", addr))
            .await
            .unwrap();
        assert_eq!(res_sub.status(), 200);
        let list_json: serde_json::Value = res_sub.json().await.unwrap();
        assert_eq!(list_json["total"], 1);
        assert_eq!(list_json["items"].as_array().unwrap().len(), 1);
        assert!(list_json["items"][0]["logs"].is_null());

        // Test 3a: list_bugs with hierarchical parent subsystem filter ("drivers" matches "drivers/net")
        let res_hier = reqwest::get(format!("http://{}/api/bugs?subsystem=drivers", addr))
            .await
            .unwrap();
        assert_eq!(res_hier.status(), 200);
        let hier_json: serde_json::Value = res_hier.json().await.unwrap();
        assert_eq!(hier_json["total"], 1);

        // Test 3b: list_bugs with non-matching subsystem filter
        let res_other_sub = reqwest::get(format!("http://{}/api/bugs?subsystem=btrfs", addr))
            .await
            .unwrap();
        assert_eq!(res_other_sub.status(), 200);
        let empty_list: serde_json::Value = res_other_sub.json().await.unwrap();
        assert_eq!(empty_list["total"], 0);

        // Test 3c: list_bugs with lifecycle filter
        let res_status = reqwest::get(format!("http://{}/api/bugs?lifecycle_status=new", addr))
            .await
            .unwrap();
        assert_eq!(res_status.status(), 200);
        let status_json: serde_json::Value = res_status.json().await.unwrap();
        assert_eq!(status_json["total"], 1);

        // Test 3d: list_bugs with lifecycle filter mismatch
        let res_open = reqwest::get(format!("http://{}/api/bugs?lifecycle_status=open", addr))
            .await
            .unwrap();
        assert_eq!(res_open.status(), 200);
        let open_json: serde_json::Value = res_open.json().await.unwrap();
        assert_eq!(open_json["total"], 0);

        // Test 3d1: the historical status spelling still selects the lifecycle
        // axis, so bookmarked URLs keep working.
        let res_alias = reqwest::get(format!("http://{}/api/bugs?status=new", addr))
            .await
            .unwrap();
        assert_eq!(res_alias.status(), 200);
        let alias_json: serde_json::Value = res_alias.json().await.unwrap();
        assert_eq!(alias_json["total"], 1);

        // Test 3d2: the pipeline axis is filterable independently.
        let res_pipeline = reqwest::get(format!("http://{}/api/bugs?pipeline_state=pending", addr))
            .await
            .unwrap();
        assert_eq!(res_pipeline.status(), 200);
        let pipeline_json: serde_json::Value = res_pipeline.json().await.unwrap();
        assert_eq!(pipeline_json["total"], 1);

        // Test 3d3: an unknown state is rejected rather than silently matching
        // nothing, which would look identical to an empty result set.
        let res_bogus = reqwest::get(format!("http://{}/api/bugs?lifecycle_status=raw", addr))
            .await
            .unwrap();
        assert_eq!(res_bogus.status(), 400);

        // Test 3e: list_bugs with sorting
        let res_sort = reqwest::get(format!(
            "http://{}/api/bugs?sort_by=severity&sort_order=desc",
            addr
        ))
        .await
        .unwrap();
        assert_eq!(res_sort.status(), 200);

        // Test 3f: list_bugs with multi-subsystem filter (subsystems parameter)
        let res_multi = reqwest::get(format!(
            "http://{}/api/bugs?subsystems=drivers/net,btrfs",
            addr
        ))
        .await
        .unwrap();
        assert_eq!(res_multi.status(), 200);
        let multi_json: serde_json::Value = res_multi.json().await.unwrap();
        assert_eq!(multi_json["total"], 1);

        // Test 3g: list_bugs with comma-separated single subsystem parameter
        let res_comma = reqwest::get(format!("http://{}/api/bugs?subsystem=drivers/net,fs", addr))
            .await
            .unwrap();
        assert_eq!(res_comma.status(), 200);
        let comma_json: serde_json::Value = res_comma.json().await.unwrap();
        assert_eq!(comma_json["total"], 1);

        // Test 3h: list_bugs with non-matching multi-subsystem
        let res_multi_none =
            reqwest::get(format!("http://{}/api/bugs?subsystems=btrfs,ext4", addr))
                .await
                .unwrap();
        assert_eq!(res_multi_none.status(), 200);
        let multi_none_json: serde_json::Value = res_multi_none.json().await.unwrap();
        assert_eq!(multi_none_json["total"], 0);

        // Test 3i: GET /api/bugs/subsystems with lifecycle_status=new
        let res_subs_api = reqwest::get(format!(
            "http://{}/api/bugs/subsystems?lifecycle_status=new",
            addr
        ))
        .await
        .unwrap();
        assert_eq!(res_subs_api.status(), 200);
        let subs_json: serde_json::Value = res_subs_api.json().await.unwrap();
        let subs_arr = subs_json.as_array().unwrap();
        assert_eq!(subs_arr.len(), 1);
        assert_eq!(subs_arr[0]["name"], "drivers/net");
        assert_eq!(subs_arr[0]["count"], 1);
        assert_eq!(subs_arr[0]["open_bugs"], 1);

        // Test 3j: GET /api/subsystems alias
        let res_subs_alias = reqwest::get(format!(
            "http://{}/api/subsystems?lifecycle_status=new",
            addr
        ))
        .await
        .unwrap();
        assert_eq!(res_subs_alias.status(), 200);

        // Test 3k: GET /api/bugs/subsystems with lifecycle_status=open (should
        // skip 0-bug entries)
        let res_subs_open = reqwest::get(format!(
            "http://{}/api/bugs/subsystems?lifecycle_status=open",
            addr
        ))
        .await
        .unwrap();
        assert_eq!(res_subs_open.status(), 200);
        let open_subs_json: serde_json::Value = res_subs_open.json().await.unwrap();
        assert_eq!(open_subs_json.as_array().unwrap().len(), 0);

        // Test 3l: duplicate linking on get_bug
        let dup_id = db
            .create_bug(&crate::db::NewBug {
                bugid: "linux-dup-endpoint".to_string(),
                title: "Duplicate issue".to_string(),
                lifecycle_status: crate::db::BugLifecycleStatus::New,
                pipeline_state: crate::db::BugPipelineState::Pending,
                assignee: None,
                reporter: "sashiko".to_string(),
                reported_at: 123457,
                discovered_in_patchset_id: None,
                discovered_in_patch_id: None,
                discovered_in_commit: None,
                source_ref: None,
                vector_json: None,
                duplicate_of_id: None,
                subsystems: vec!["drivers/net".to_string()],
            })
            .await
            .unwrap();
        db.mark_bug_as_duplicate(crate::db::MarkDuplicateBugParams {
            ephemeral_id: dup_id,
            canonical_id: bug_id,
            reasoning: "Dup of test_device",
            logs: None,
            tokens_in: None,
            tokens_out: None,
            tokens_cached: None,
        })
        .await
        .unwrap();

        // Check duplicate_of on the duplicate bug
        let res_dup = reqwest::get(format!("http://{}/api/bug?id={}", addr, dup_id))
            .await
            .unwrap();
        assert_eq!(res_dup.status(), 200);
        let dup_resp: serde_json::Value = res_dup.json().await.unwrap();
        assert_eq!(dup_resp["duplicate_of"]["id"], bug_id);
        assert_eq!(dup_resp["duplicate_of"]["bugid"], "linux-12345678");

        // Check duplicates array on canonical bug
        let res_canon = reqwest::get(format!("http://{}/api/bug?id={}", addr, bug_id))
            .await
            .unwrap();
        assert_eq!(res_canon.status(), 200);
        let canon_resp: serde_json::Value = res_canon.json().await.unwrap();
        assert_eq!(canon_resp["duplicates"].as_array().unwrap().len(), 1);
        assert_eq!(canon_resp["duplicates"][0]["id"], dup_id);

        // Test 4: redirect_bug
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let res = client
            .get(format!("http://{}/bug/pb-12345678", addr))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 307);
        assert_eq!(
            res.headers().get("location").unwrap().to_str().unwrap(),
            "/#/bug/pb-12345678"
        );

        // Test 5: bug_action
        let action_res = reqwest::Client::new()
            .post(format!("http://{}/api/bug/action?id={}", addr, bug_id))
            .json(&serde_json::json!({
                "action": "comment",
                "content": "A new test comment via API"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(action_res.status(), 200);

        let token = crate::auth::create_token(
            "maintainer@example.org",
            settings.server.jwt_secret.as_deref().unwrap(),
            None,
            3600,
        )
        .unwrap();
        let close = reqwest::Client::new()
            .post(format!("http://{}/api/bug/action?id={}", addr, bug_id))
            .bearer_auth(token)
            .json(&serde_json::json!({"action":"close", "reason":"Fixed upstream", "tool":"web"}))
            .send()
            .await
            .unwrap();
        assert_eq!(close.status(), 200);
        let stored = db.get_bug(bug_id).await.unwrap().unwrap();
        let comment = stored
            .enrichments
            .iter()
            .find(|e| e.content.as_deref() == Some("Fixed upstream"))
            .unwrap();
        assert_eq!(comment.author.as_deref(), Some("maintainer@example.org"));
        assert_eq!(comment.tool, "web");
        assert!(comment.model.is_none());
        let raw: serde_json::Value =
            reqwest::get(format!("http://{}/api/bug/raw?id={}", addr, bug_id))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(raw["records"].as_array().unwrap().len(), 2);
        assert!(
            raw["records"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|b| b["enrichments"].as_array().unwrap())
                .any(|e| e["logs"].as_str().is_some_and(|s| s.contains("test")))
        );
        let missing = reqwest::get(format!("http://{}/api/bug/raw?id=999999", addr))
            .await
            .unwrap();
        assert_eq!(missing.status(), 404);

        // Test 6: get_config reports permissions, and remote unauthenticated action is denied with clear message
        let cfg_loopback: serde_json::Value = reqwest::get(format!("http://{}/api/config", addr))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(cfg_loopback["permissions"]["action"], true);

        let client = reqwest::Client::new();
        let cfg_remote: serde_json::Value = client
            .get(format!("http://{}/api/config", addr))
            .header("x-forwarded-for", "203.0.113.1")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(cfg_remote["permissions"]["action"], false);

        let remote_denied = client
            .post(format!("http://{}/api/bug/action?id={}", addr, bug_id))
            .header("x-forwarded-for", "203.0.113.1")
            .json(&serde_json::json!({
                "action": "comment",
                "content": "Attempt by remote guest"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(remote_denied.status(), 403);
        let err_msg = remote_denied.text().await.unwrap();
        assert_eq!(
            err_msg,
            "You don't have permissions to perform bug actions."
        );
    }

    #[tokio::test]
    async fn test_acl_blocklist_authorization_and_auth_endpoints() {
        let db_settings = crate::settings::DatabaseSettings {
            url: ":memory:".to_string(),
            token: String::new(),
        };
        let db = Arc::new(Database::new(&db_settings).await.unwrap());
        db.migrate().await.unwrap();

        let mut base_settings = crate::settings::Settings::new().unwrap();
        base_settings.server.jwt_secret = Some("test_jwt_secret_12345678901234567890".to_string());
        base_settings.server.testing_mode = false;
        base_settings.server.acl.admins = vec!["admin@example.com".to_string()];
        base_settings.server.acl.review = vec!["reviewer@example.com".to_string()];
        base_settings.server.acl.blocklist = vec![
            "blocked@example.com".to_string(),
            "admin@example.com".to_string(),
        ];

        let settings = Arc::new(base_settings);
        let (event_tx, _event_rx) = mpsc::channel(10);
        let (fetch_tx, _fetch_rx) = mpsc::channel(10);

        let app = build_router(
            settings.clone(),
            db.clone(),
            event_tx.clone(),
            fetch_tx.clone(),
            false,
            false,
            true,
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let client = reqwest::Client::new();

        // 1. request_link for blocklisted email (even though it's in admins) -> 403 Forbidden
        let res_blocked_admin = client
            .post(format!("http://{}/api/auth/request-link", addr))
            .json(&serde_json::json!({ "email": "admin@example.com" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res_blocked_admin.status(), 403);

        // 2. request_link for blocklisted email with case variations -> 403 Forbidden
        let res_blocked_case = client
            .post(format!("http://{}/api/auth/request-link", addr))
            .json(&serde_json::json!({ "email": "BLOCKED@example.com" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res_blocked_case.status(), 403);

        // 3. request_link for allowed email -> 200 OK
        let res_allowed = client
            .post(format!("http://{}/api/auth/request-link", addr))
            .json(&serde_json::json!({ "email": "reviewer@example.com" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res_allowed.status(), 200);

        // 4. verify_link for blocklisted email -> 403 Forbidden
        let secret = "test_jwt_secret_12345678901234567890";
        let blocked_token = crate::auth::create_token(
            "blocked@example.com",
            secret,
            Some("sign_in_link".to_string()),
            1800,
        )
        .unwrap();

        let res_verify_blocked = client
            .get(format!(
                "http://{}/api/auth/verify?token={}",
                addr, blocked_token
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(res_verify_blocked.status(), 403);

        // 5. verify_link for allowed email -> 200 OK and returns session token
        let allowed_token = crate::auth::create_token(
            "reviewer@example.com",
            secret,
            Some("sign_in_link".to_string()),
            1800,
        )
        .unwrap();

        let res_verify_allowed = client
            .get(format!(
                "http://{}/api/auth/verify?token={}",
                addr, allowed_token
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(res_verify_allowed.status(), 200);
        let session_json: serde_json::Value = res_verify_allowed.json().await.unwrap();
        let session_token = session_json["token"].as_str().unwrap();

        // 6. refresh_token for blocklisted email session -> 403 Forbidden
        let blocked_session_token = crate::auth::create_token(
            "blocked@example.com",
            secret,
            Some("session".to_string()),
            86400,
        )
        .unwrap();

        unsafe {
            std::env::set_var("JWT_SECRET", secret);
        }

        let res_refresh_blocked = client
            .post(format!("http://{}/api/auth/refresh", addr))
            .header("Authorization", format!("Bearer {}", blocked_session_token))
            .send()
            .await
            .unwrap();
        assert_eq!(res_refresh_blocked.status(), 403);

        // 7. refresh_token for allowed email session -> 200 OK
        let res_refresh_allowed = client
            .post(format!("http://{}/api/auth/refresh", addr))
            .header("Authorization", format!("Bearer {}", session_token))
            .send()
            .await
            .unwrap();
        assert_eq!(res_refresh_allowed.status(), 200);

        // 8. Test is_authorized directly:
        let mut proxy_headers = axum::http::HeaderMap::new();
        proxy_headers.insert("x-forwarded-for", "203.0.113.195".parse().unwrap());
        let dummy_addr = "127.0.0.1:12345".parse().unwrap();
        let state_arc = Arc::new(AppState {
            settings: settings.clone(),
            db: db.clone(),
            sender: event_tx,
            fetch_sender: fetch_tx,
            read_only: false,
            forge_registry: Arc::new(crate::forge::ForgeRegistry::new()),
            allow_all_submit: false,
            smtp_enabled: false,
            dry_run: true,
            stats_timeline_cache: AsyncMapCache::new(Duration::from_secs(60)),
            stats_reviews_cache: AsyncCache::new(Duration::from_secs(60)),
            stats_tools_cache: AsyncCache::new(Duration::from_secs(60)),
            messages_count_cache: AsyncCache::new(Duration::from_secs(30)),
            patchsets_count_cache: AsyncCache::new(Duration::from_secs(30)),
            patchsets_homepage_cache: AsyncCache::new(Duration::from_secs(10)),
            messages_homepage_cache: AsyncCache::new(Duration::from_secs(10)),
            bug_subsystems_cache: AsyncMapCache::new(Duration::from_secs(5)),
        });

        let blocked_user = crate::auth::AuthUser {
            email: "blocked@example.com".to_string(),
        };
        assert!(!is_authorized(
            &dummy_addr,
            &state_arc,
            &proxy_headers,
            Some(&blocked_user),
            crate::settings::Permission::Review
        ));

        let empty_headers = axum::http::HeaderMap::new();
        assert!(!is_authorized(
            &dummy_addr,
            &state_arc,
            &empty_headers,
            Some(&blocked_user),
            crate::settings::Permission::Review
        ));

        let allowed_user = crate::auth::AuthUser {
            email: "reviewer@example.com".to_string(),
        };
        assert!(is_authorized(
            &dummy_addr,
            &state_arc,
            &proxy_headers,
            Some(&allowed_user),
            crate::settings::Permission::Review
        ));
    }
}

pub fn is_authorized(
    addr: &std::net::SocketAddr,
    state: &std::sync::Arc<AppState>,
    headers: &axum::http::HeaderMap,
    auth: Option<&crate::auth::AuthUser>,
    perm: crate::settings::Permission,
) -> bool {
    if state.settings.server.testing_mode {
        return true;
    }
    if state.allow_all_submit {
        return true;
    }
    if auth.is_some_and(|user| state.settings.server.acl.is_blocklisted(&user.email)) {
        return false;
    }
    if addr.ip().to_canonical().is_loopback() {
        let has_proxy = headers.contains_key("x-forwarded-for")
            || headers.contains_key("x-real-ip")
            || headers.contains_key("forwarded");
        if !has_proxy {
            return true;
        }
    }

    if let Some(user) = auth {
        return state.settings.server.acl.has_permission(&user.email, perm);
    }

    false
}

#[derive(serde::Deserialize)]
struct RequestLinkRequest {
    email: String,
}

async fn request_link(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Json(payload): axum::extract::Json<RequestLinkRequest>,
) -> Result<StatusCode, StatusCode> {
    if let Some(secret) = &state.settings.server.jwt_secret {
        // Enforce that only identities explicitly configured in our ACL get sign-in links sent to them
        let acl = &state.settings.server.acl;
        if acl.is_blocklisted(&payload.email) {
            tracing::warn!(
                "Login attempt denied for blocklisted identity: {}",
                payload.email
            );
            return Err(StatusCode::FORBIDDEN);
        }
        let is_known = acl
            .admins
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&payload.email))
            || acl
                .ingest
                .iter()
                .any(|e| e.eq_ignore_ascii_case(&payload.email))
            || acl
                .cancel
                .iter()
                .any(|e| e.eq_ignore_ascii_case(&payload.email))
            || acl
                .review
                .iter()
                .any(|e| e.eq_ignore_ascii_case(&payload.email))
            || acl
                .action
                .iter()
                .any(|e| e.eq_ignore_ascii_case(&payload.email));

        if !is_known {
            tracing::warn!(
                "Unauthorized login attempt for unknown identity: {}",
                payload.email
            );
            return Err(StatusCode::FORBIDDEN);
        }
        let token =
            crate::auth::create_token(&payload.email, secret, Some("sign_in_link".to_string()), 1800)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        tracing::info!(
            "SIGN-IN LINK REQUESTED for {}: http://{}:{}/?magic_token={}",
            payload.email,
            state.settings.server.host,
            state.settings.server.port,
            token
        );
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_IMPLEMENTED)
    }
}

#[derive(serde::Deserialize)]
struct VerifyLinkQuery {
    token: String,
}

async fn verify_link(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Query(query): axum::extract::Query<VerifyLinkQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, &'static str)> {
    if let Some(secret) = &state.settings.server.jwt_secret {
        let claims = crate::auth::verify_token(&query.token, secret)
            .map_err(|_| (StatusCode::UNAUTHORIZED, "Invalid sign-in link"))?;
        if claims.typ.as_deref() != Some("sign_in_link") {
            return Err((StatusCode::UNAUTHORIZED, "Invalid token type"));
        }
        if state.settings.server.acl.is_blocklisted(&claims.sub) {
            return Err((StatusCode::FORBIDDEN, "User is blocklisted"));
        }
        let session_token =
            crate::auth::create_token(&claims.sub, secret, Some("session".to_string()), 86400)
                .map_err(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to create session",
                    )
                })?;
        Ok(Json(serde_json::json!({ "token": session_token })))
    } else {
        Err((StatusCode::NOT_IMPLEMENTED, "JWT not configured"))
    }
}

async fn refresh_token(
    auth: crate::auth::OptionalAuthUser,
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, &'static str)> {
    if let Some(secret) = &state.settings.server.jwt_secret {
        if let Some(user) = auth.0 {
            if state.settings.server.acl.is_blocklisted(&user.email) {
                return Err((StatusCode::FORBIDDEN, "User is blocklisted"));
            }
            let session_token =
                crate::auth::create_token(&user.email, secret, Some("session".to_string()), 86400)
                    .map_err(|_| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Failed to create session",
                        )
                    })?;
            Ok(Json(serde_json::json!({ "token": session_token })))
        } else {
            Err((StatusCode::UNAUTHORIZED, "Missing or invalid token"))
        }
    } else {
        Err((StatusCode::NOT_IMPLEMENTED, "JWT not configured"))
    }
}
