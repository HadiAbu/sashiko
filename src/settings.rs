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

use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct SubsystemMapping {
    pub pattern: String,
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct SubsystemsSettings {
    #[serde(default)]
    pub mapping: Vec<SubsystemMapping>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct ProjectSettings {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub attribution: Option<String>,
}

impl ProjectSettings {
    pub fn attribution(&self) -> &str {
        if let Some(ref attr) = self.attribution {
            attr.as_str()
        } else if !self.domain.is_empty() {
            self.domain.as_str()
        } else {
            "sashiko"
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct ForgeSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub disable_nntp: bool,
    pub provider: Option<String>,
    pub webhook_secret: Option<String>,
    pub api_token: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct DatabaseSettings {
    pub url: String,
    pub token: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct NntpSettings {
    pub server: String,
    pub port: u16,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct SmtpSettings {
    pub server: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub sender_address: String,
    pub reply_to: Option<String>,
    #[serde(default = "default_dry_run")]
    pub dry_run: bool,
}

fn default_dry_run() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct MailingListsSettings {
    #[serde(deserialize_with = "deserialize_string_or_vec")]
    pub track: Vec<String>,
}

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> serde::de::Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("string or list of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect())
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(elem) = seq.next_element()? {
                vec.push(elem);
            }
            Ok(vec)
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

fn default_max_input_tokens() -> usize {
    150_000
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct ClaudeSettings {
    #[serde(default = "default_prompt_caching")]
    pub prompt_caching: bool,
    #[serde(default = "default_claude_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

fn default_claude_max_tokens() -> u32 {
    4096
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct GeminiSettings {
    #[serde(default)]
    pub explicit_prompt_caching: bool,
}

#[cfg(feature = "bedrock")]
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct BedrockSettings {
    /// AWS region for Bedrock API calls (e.g. "us-east-1").
    /// If omitted, uses the standard AWS SDK default chain.
    pub region: Option<String>,
    #[serde(default = "default_prompt_caching")]
    pub prompt_caching: bool,
    /// Max output tokens per Converse call.
    #[serde(default = "default_bedrock_max_tokens")]
    pub max_tokens: u32,
    /// Thinking mode sent as additional_model_request_fields. Opus 4.7 only accepts "adaptive".
    /// Leave unset to omit (thinking disabled). Valid values: "adaptive".
    #[serde(default)]
    pub thinking: Option<String>,
    /// output_config.effort level. Valid values: "low", "medium", "high", "xhigh", "max".
    /// Leave unset to use the model default. "xhigh" is Opus 4.7-only.
    #[serde(default)]
    pub effort: Option<String>,
}

#[cfg(feature = "bedrock")]
fn default_bedrock_max_tokens() -> u32 {
    8192
}

fn default_prompt_caching() -> bool {
    true
}

#[cfg(feature = "vertex")]
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct VertexSettings {
    /// GCP project ID. Falls back to ANTHROPIC_VERTEX_PROJECT_ID env var.
    #[serde(default)]
    pub project_id: Option<String>,
    /// GCP region (e.g., "us-east5", "global"). Falls back to CLOUD_ML_REGION env var.
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default = "default_prompt_caching")]
    pub prompt_caching: bool,
    #[serde(default = "default_vertex_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

#[cfg(feature = "vertex")]
fn default_vertex_max_tokens() -> u32 {
    8192
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct OpenAiCompatSettings {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub context_window_size: Option<usize>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct VllmSettings {
    #[serde(default)]
    pub base_url: Option<String>,
    /// Should match the server-side `--max-model-len`.
    #[serde(default)]
    pub context_window_size: Option<usize>,
    /// Completion token limit. Leave unset to let vLLM generate up to the
    /// remaining context (`max_model_len - prompt_tokens`).
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Enable or disable thinking for reasoning models (e.g. Qwen3) via
    /// `chat_template_kwargs`. Leave unset for the model default.
    #[serde(default)]
    pub enable_thinking: Option<bool>,
    /// Enforce JSON responses with guided decoding (`response_format`).
    /// Disabled by default because not every vLLM backend supports it;
    /// without it the JSON requirement is injected into the system prompt.
    #[serde(default)]
    pub guided_json: bool,
    /// Forward tool definitions to the server. Disabled by default because a
    /// server started without `--enable-auto-tool-choice` and
    /// `--tool-call-parser` rejects requests carrying tools with HTTP 400.
    #[serde(default)]
    pub enable_tools: bool,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct OllamaSettings {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub context_window_size: Option<usize>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub think: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct KiroCliSettings {
    #[serde(default = "default_kiro_cli_binary")]
    pub binary: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default = "default_kiro_cli_context_window")]
    pub context_window_size: usize,
}

fn default_kiro_cli_binary() -> String {
    "kiro-cli".to_string()
}

fn default_kiro_cli_context_window() -> usize {
    200_000
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct ClaudeCliSettings {
    /// Effort level passed to `claude --effort`. Valid values per Claude Code:
    /// "low", "medium", "high", "xhigh", "max". Leave unset for the model default.
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct CodexCliSettings {
    /// Reasoning effort passed as `codex exec -c model_reasoning_effort=<v>`.
    /// Valid values: "none", "minimal", "low", "medium", "high", "xhigh",
    /// "max". Leave unset for the account default. A `-c` override outranks
    /// `~/.codex/config.toml`, but not an enterprise-managed requirements
    /// layer, which substitutes its own value whatever the origin. A run
    /// whose effort that layer substitutes fails.
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct DevinCliSettings {
    /// Path to a Devin declarative agent config file (JSON or YAML) passed via
    /// `--agent-config`. Use this to disable all tools for a strictly
    /// text-completion backend.
    #[serde(default)]
    pub agent_config: Option<String>,
    /// Path to a Devin config file passed via `--config`. Use this to apply
    /// custom permission rules (e.g. deny-all) for the provider session
    /// without polluting the user's `~/.config/devin/config.json`.
    #[serde(default)]
    pub config: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct AiSettings {
    pub provider: String,
    pub model: String,
    #[serde(default = "default_max_input_tokens")]
    pub max_input_tokens: usize,
    #[serde(default = "default_max_interactions")]
    pub max_interactions: usize,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_api_timeout_secs")]
    pub api_timeout_secs: u64,
    #[serde(skip, default)]
    pub no_ai: bool,
    /// Log each AI request/response turn at info level (content previews + token counts).
    /// Useful for debugging but verbose; disabled by default.
    #[serde(default)]
    pub log_turns: bool,
    #[serde(default)]
    pub response_cache: bool,
    #[serde(default = "default_response_cache_ttl_days")]
    pub response_cache_ttl_days: u64,
    // Provider-specific settings
    pub claude: Option<ClaudeSettings>,
    pub gemini: Option<GeminiSettings>,
    #[cfg(feature = "bedrock")]
    pub bedrock: Option<BedrockSettings>,
    #[cfg(feature = "vertex")]
    pub vertex: Option<VertexSettings>,
    pub openai_compat: Option<OpenAiCompatSettings>,
    pub ollama: Option<OllamaSettings>,
    pub vllm: Option<VllmSettings>,
    pub kiro_cli: Option<KiroCliSettings>,
    pub claude_cli: Option<ClaudeCliSettings>,
    pub codex_cli: Option<CodexCliSettings>,
    pub devin_cli: Option<DevinCliSettings>,
}

fn default_response_cache_ttl_days() -> u64 {
    7
}

fn default_api_timeout_secs() -> u64 {
    300
}

fn default_temperature() -> f32 {
    1.0
}

fn default_max_interactions() -> usize {
    100
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Ingest,
    Cancel,
    Review,
    Action,
}

impl Permission {
    /// Whether a credential-free request from a loopback address may exercise
    /// this capability.
    ///
    /// The bypass exists so a developer running the server locally can drive it
    /// without configuring a JWT secret, and it is tolerable only where the
    /// blast radius is that local instance. Authority over a Linux kernel bug
    /// is deliberately not a Permission: it is resolved per bug by
    /// BugPrincipal, which never reaches this path, so no amount of local
    /// access opens the bug database.
    ///
    /// The match is exhaustive rather than defaulted so that a capability
    /// added later is not bypassable until someone writes it down here.
    pub fn allows_loopback_bypass(self) -> bool {
        match self {
            Permission::Ingest => true,
            Permission::Cancel => true,
            Permission::Review => true,
            Permission::Action => true,
        }
    }
}

/// Access Control List settings utilizing fine-grained capability endpoints.
/// By default (if omitted), all vectors are safely initialized empty (Fail-Closed).
/// Users must explicitly be added to the necessary capability lists to perform mutations.
/// The `blocklist` explicitly denies all capabilities, overriding any grants.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct AclSettings {
    #[serde(default)]
    pub admins: Vec<String>,
    /// The kernel security list. Reads and comments on every bug, and reads
    /// the raw analysis transcripts, without gaining any of the capabilities
    /// below.
    #[serde(default)]
    pub security: Vec<String>,
    /// Principals allowed to file a bug over HTTP. Empty means only operators
    /// can, which is the shipped configuration.
    #[serde(default)]
    pub bug_reporters: Vec<String>,
    #[serde(default)]
    pub ingest: Vec<String>,
    #[serde(default)]
    pub cancel: Vec<String>,
    #[serde(default)]
    pub review: Vec<String>,
    #[serde(default)]
    pub action: Vec<String>,
    #[serde(default)]
    pub blocklist: Vec<String>,
}

/// Matches an address against a capability list.
///
/// Both sides are trimmed and compared case insensitively: the list is written
/// by hand in a configuration file and the address arrives from a sign-in
/// form, so a stray space on either side must not decide who gets in or,
/// worse, let a blocklisted address slip past.
fn list_contains(list: &[String], email: &str) -> bool {
    let email = email.trim();
    list.iter().any(|e| e.trim().eq_ignore_ascii_case(email))
}

impl AclSettings {
    pub fn is_blocklisted(&self, email: &str) -> bool {
        list_contains(&self.blocklist, email)
    }

    /// Whether the address is an operator. Operators are never blocklisted
    /// implicitly; the caller checks the blocklist first.
    pub fn is_admin(&self, email: &str) -> bool {
        list_contains(&self.admins, email)
    }

    /// Whether the address is on the kernel security list.
    pub fn is_security(&self, email: &str) -> bool {
        list_contains(&self.security, email)
    }

    /// Whether the address may file a bug over HTTP.
    pub fn is_bug_reporter(&self, email: &str) -> bool {
        list_contains(&self.bug_reporters, email)
    }

    pub fn has_permission(&self, email: &str, perm: Permission) -> bool {
        if self.is_blocklisted(email) {
            return false;
        }
        if self.is_admin(email) {
            return true;
        }
        match perm {
            Permission::Ingest => list_contains(&self.ingest, email),
            Permission::Cancel => list_contains(&self.cancel, email),
            Permission::Review => list_contains(&self.review, email),
            Permission::Action => list_contains(&self.action, email),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub testing_mode: bool,
    pub jwt_secret: Option<String>,
    #[serde(default)]
    pub acl: AclSettings,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct CustomRemoteSettings {
    pub name: String,
    pub url: String,
    pub check_all_branches: bool,
    pub only_branches: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct GitSettings {
    pub repository_path: String,
    pub custom_remotes: Option<Vec<CustomRemoteSettings>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct ReviewSettings {
    pub concurrency: usize,
    pub worktree_dir: String,
    #[serde(default = "default_review_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_max_lines_changed")]
    pub max_lines_changed: usize,
    #[serde(default = "default_max_files_touched")]
    pub max_files_touched: usize,
    #[serde(default)]
    pub ignore_files: Vec<String>,
    #[serde(default = "default_email_policy_path")]
    pub email_policy_path: String,
    /// Maximum cumulative non-cached tokens (uncached input + output) across all turns in a
    /// single review. Cached input tokens are excluded because they cost ~10x less and don't
    /// reflect runaway model behaviour. At Sonnet 4.6 pricing ($3/M uncached input, $15/M
    /// output) the 5M default costs roughly $15–75 depending on input/output mix; a typical
    /// 7-stage review uses ~300–500k tokens total. Set to 0 to disable.
    #[serde(default = "default_max_total_tokens")]
    pub max_total_tokens: usize,
    /// Maximum cumulative output tokens across all turns in a single review.
    /// Conservative default; set to 0 to disable.
    #[serde(default = "default_max_total_output_tokens")]
    pub max_total_output_tokens: usize,
    #[serde(skip)]
    pub stages: Option<Vec<String>>,
}

fn default_max_total_tokens() -> usize {
    5_000_000
}

fn default_max_total_output_tokens() -> usize {
    500_000
}

fn default_max_lines_changed() -> usize {
    10_000
}

fn default_max_files_touched() -> usize {
    200
}

fn default_review_timeout() -> u64 {
    3600
}

fn default_max_retries() -> u32 {
    3
}

fn default_email_policy_path() -> String {
    "email_policy.toml".to_string()
}

/// Tuning for the Linux bug analysis worker.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct LinuxBugSettings {
    /// How long a claimed bug stays claimed before another worker may take it
    /// over. This must comfortably exceed the longest expected analysis, or a
    /// slow run will be reclaimed and analysed twice.
    #[serde(default = "default_bug_lease_ttl_seconds")]
    pub lease_ttl_seconds: i64,
    /// How many times a bug may be analysed before it is abandoned. Without a
    /// cap, a bug that reliably crashes the worker is retried forever.
    #[serde(default = "default_bug_max_attempts")]
    pub max_attempts: i64,
}

impl Default for LinuxBugSettings {
    fn default() -> Self {
        Self {
            lease_ttl_seconds: default_bug_lease_ttl_seconds(),
            max_attempts: default_bug_max_attempts(),
        }
    }
}

fn default_bug_lease_ttl_seconds() -> i64 {
    1800
}

fn default_bug_max_attempts() -> i64 {
    3
}

fn default_log_level() -> String {
    "info".to_string()
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[allow(unused)]
pub struct Settings {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub project: ProjectSettings,
    #[serde(default = "default_subsystems")]
    pub subsystems: SubsystemsSettings,
    #[serde(default = "default_forge")]
    pub forge: ForgeSettings,
    pub database: DatabaseSettings,
    pub nntp: NntpSettings,
    pub smtp: Option<SmtpSettings>,
    pub mailing_lists: MailingListsSettings,
    pub ai: AiSettings,
    pub server: ServerSettings,
    pub git: GitSettings,
    pub review: ReviewSettings,
    #[serde(default)]
    pub linux_bug: LinuxBugSettings,
}

fn default_subsystems() -> SubsystemsSettings {
    SubsystemsSettings { mapping: vec![] }
}

fn default_forge() -> ForgeSettings {
    ForgeSettings {
        enabled: false,
        disable_nntp: true,
        provider: None,
        webhook_secret: None,
        api_token: None,
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct LocalReviewReviewSettings {
    pub concurrency: usize,
    #[serde(default = "default_review_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LocalReviewSettings {
    pub ai: AiSettings,
    pub review: LocalReviewReviewSettings,
}
impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        Self::from_file("Settings")
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let s = Config::builder()
            // Start with default settings
            .add_source(File::from(path.as_ref()))
            // Add settings from environment variables (with a prefix of SASHIKO)
            // e.g. SASHIKO__SERVER__PORT=8081 would set the server port
            .add_source(Environment::with_prefix("SASHIKO").separator("__"))
            .build()?;

        s.try_deserialize()
    }

    pub fn local_review_path() -> PathBuf {
        Self::local_review_path_in(Path::new("."))
    }

    pub fn local_review_path_in(base: &Path) -> PathBuf {
        let local = base.join("Settings.toml");
        if local.exists() {
            return local;
        }

        Self::user_config_path()
    }

    pub fn user_config_path() -> PathBuf {
        if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(config_home).join("sashiko.toml");
        }

        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config/sashiko.toml");
        }

        PathBuf::from(".config/sashiko.toml")
    }

    pub fn local_review() -> Result<Self, ConfigError> {
        Self::from_file(Self::local_review_path())
    }

    pub fn local_review_settings() -> Result<LocalReviewSettings, ConfigError> {
        Self::local_review_from_file(Self::local_review_path())
    }

    pub fn local_review_from_file(
        path: impl AsRef<Path>,
    ) -> Result<LocalReviewSettings, ConfigError> {
        let s = Config::builder()
            .add_source(File::from(path.as_ref()))
            .add_source(Environment::with_prefix("SASHIKO").separator("__"))
            .build()?;

        s.try_deserialize()
    }

    pub fn local_review_ai() -> Result<AiSettings, ConfigError> {
        Self::ai_from_file(Self::local_review_path())
    }

    pub fn ai_from_file(path: impl AsRef<Path>) -> Result<AiSettings, ConfigError> {
        let s = Config::builder()
            .add_source(File::from(path.as_ref()))
            .add_source(Environment::with_prefix("SASHIKO").separator("__"))
            .build()?;

        let settings: LocalReviewSettings = s.try_deserialize()?;
        Ok(settings.ai)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_production_settings_is_valid() {
        let path = "Settings.toml";
        if Path::new(path).exists() {
            let _ = Settings::from_file("Settings")
                .expect("Production 'Settings.toml' failed to parse");
        }
    }

    #[test]
    fn test_project_settings_attribution_and_domain() {
        let default_proj = ProjectSettings::default();
        assert_eq!(default_proj.domain, "");
        assert_eq!(default_proj.attribution(), "sashiko");

        let toml_default: ProjectSettings = toml::from_str("name = \"Test\"").unwrap();
        assert_eq!(toml_default.domain, "");
        assert_eq!(toml_default.attribution(), "sashiko");

        let toml_domain: ProjectSettings = toml::from_str("domain = \"sashiko.dev\"").unwrap();
        assert_eq!(toml_domain.domain, "sashiko.dev");
        assert_eq!(toml_domain.attribution(), "sashiko.dev");

        let toml_attr: ProjectSettings =
            toml::from_str("domain = \"custom.org\"\nattribution = \"custom-team\"").unwrap();
        assert_eq!(toml_attr.attribution(), "custom-team");
    }

    /// concurrency is required for local reviews exactly as it is for the
    /// daemon, so an absent [review] section is an error rather than a guess
    /// at how much machine the review has to itself.
    #[test]
    fn test_local_review_requires_a_review_section() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Settings.toml");
        let ai = "[ai]\nprovider = \"gemini\"\nmodel = \"gemini-3-pro\"\n";

        std::fs::write(&path, ai).unwrap();
        assert!(Settings::local_review_from_file(&path).is_err());

        std::fs::write(&path, format!("{}\n[review]\nconcurrency = 8\n", ai)).unwrap();
        let settings = Settings::local_review_from_file(&path).unwrap();
        assert_eq!(settings.review.concurrency, 8);
        // timeout_seconds keeps a default, as it does for the daemon.
        assert_eq!(settings.review.timeout_seconds, 3600);
    }

    /// `sashiko init` writes this template, so it has to satisfy the shape a
    /// local review reads or the two commands disagree out of the box.
    #[test]
    fn test_init_template_satisfies_local_review() {
        Settings::local_review_from_file("docs/examples/Settings.example.toml")
            .expect("init template must parse as local review settings");
    }

    #[test]
    fn test_local_review_path_prefers_current_directory() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("Settings.toml"), "").unwrap();
        assert_eq!(
            Settings::local_review_path_in(temp.path()),
            temp.path().join("Settings.toml")
        );
    }

    #[test]
    fn test_user_config_path_uses_xdg_config_home() {
        let temp = tempfile::tempdir().unwrap();
        let old_xdg = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", temp.path());
        }

        assert_eq!(
            Settings::user_config_path(),
            temp.path().join("sashiko.toml")
        );

        unsafe {
            if let Some(value) = old_xdg {
                std::env::set_var("XDG_CONFIG_HOME", value);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
        }
    }

    #[test]
    fn test_acl_default_fails_closed() {
        let acl = AclSettings::default();
        let email = "user@example.com";
        assert!(!acl.is_blocklisted(email));
        assert!(!acl.has_permission(email, Permission::Ingest));
        assert!(!acl.has_permission(email, Permission::Cancel));
        assert!(!acl.has_permission(email, Permission::Review));
        assert!(!acl.has_permission(email, Permission::Action));
    }

    #[test]
    fn test_acl_admin_grants_all_permissions() {
        let acl = AclSettings {
            admins: vec!["admin@example.com".to_string()],
            ..Default::default()
        };
        assert!(acl.has_permission("admin@example.com", Permission::Ingest));
        assert!(acl.has_permission("admin@example.com", Permission::Cancel));
        assert!(acl.has_permission("admin@example.com", Permission::Review));
        assert!(acl.has_permission("admin@example.com", Permission::Action));
    }

    #[test]
    fn test_acl_granular_capabilities() {
        let acl = AclSettings {
            ingest: vec!["bot@example.com".to_string()],
            cancel: vec!["cron@example.com".to_string()],
            review: vec!["reviewer@example.com".to_string()],
            action: vec!["collab@example.com".to_string()],
            ..Default::default()
        };

        assert!(acl.has_permission("bot@example.com", Permission::Ingest));
        assert!(!acl.has_permission("bot@example.com", Permission::Cancel));
        assert!(!acl.has_permission("bot@example.com", Permission::Review));
        assert!(!acl.has_permission("bot@example.com", Permission::Action));

        assert!(acl.has_permission("reviewer@example.com", Permission::Review));
        assert!(!acl.has_permission("reviewer@example.com", Permission::Ingest));
    }

    #[test]
    fn test_acl_blocklist_preempts_all_capabilities_and_admin() {
        let acl = AclSettings {
            admins: vec!["rogue_admin@example.com".to_string()],
            ingest: vec!["rogue_admin@example.com".to_string()],
            cancel: vec!["rogue_admin@example.com".to_string()],
            review: vec!["rogue_admin@example.com".to_string()],
            action: vec!["rogue_admin@example.com".to_string()],
            blocklist: vec!["rogue_admin@example.com".to_string()],
            ..Default::default()
        };

        assert!(acl.is_blocklisted("rogue_admin@example.com"));
        assert!(!acl.has_permission("rogue_admin@example.com", Permission::Ingest));
        assert!(!acl.has_permission("rogue_admin@example.com", Permission::Cancel));
        assert!(!acl.has_permission("rogue_admin@example.com", Permission::Review));
        assert!(!acl.has_permission("rogue_admin@example.com", Permission::Action));
    }

    #[test]
    fn test_acl_blocklist_case_insensitivity() {
        let acl = AclSettings {
            admins: vec!["User@Example.COM".to_string()],
            blocklist: vec!["User@Example.COM".to_string()],
            ..Default::default()
        };

        assert!(acl.is_blocklisted("user@example.com"));
        assert!(acl.is_blocklisted("USER@EXAMPLE.COM"));
        assert!(acl.is_blocklisted("uSeR@eXaMpLe.CoM"));
        assert!(!acl.has_permission("user@example.com", Permission::Review));
        assert!(!acl.has_permission("USER@EXAMPLE.COM", Permission::Review));
    }

    #[test]
    fn test_acl_deserialization_with_blocklist() {
        let toml_blocklist = r#"
            admins = ["alice@example.com"]
            blocklist = ["mallory@example.com"]
        "#;
        let acl: AclSettings = toml::from_str(toml_blocklist).expect("deserialization failed");
        assert_eq!(acl.blocklist, vec!["mallory@example.com"]);
        assert!(acl.is_blocklisted("mallory@example.com"));
    }

    #[test]
    fn test_security_list_grants_no_capabilities() {
        let acl = AclSettings {
            security: vec!["gregkh@linuxfoundation.org".to_string()],
            ..Default::default()
        };
        assert!(acl.is_security("gregkh@linuxfoundation.org"));
        // Membership is about bugs. It must not leak into the capabilities
        // that spend money or move patches around.
        for perm in [
            Permission::Ingest,
            Permission::Cancel,
            Permission::Review,
            Permission::Action,
        ] {
            assert!(!acl.has_permission("gregkh@linuxfoundation.org", perm));
        }
        assert!(!acl.is_admin("gregkh@linuxfoundation.org"));
        assert!(!acl.is_bug_reporter("gregkh@linuxfoundation.org"));
    }

    #[test]
    fn test_bug_reporters_defaults_to_nobody() {
        let acl = AclSettings::default();
        assert!(!acl.is_bug_reporter("tool@example.com"));

        let acl = AclSettings {
            bug_reporters: vec!["tool@example.com".to_string()],
            ..Default::default()
        };
        assert!(acl.is_bug_reporter("tool@example.com"));
        assert!(!acl.is_security("tool@example.com"));
    }

    #[test]
    fn test_list_matching_tolerates_surrounding_whitespace() {
        let acl = AclSettings {
            security: vec![" gregkh@linuxfoundation.org ".to_string()],
            blocklist: vec!["  mallory@example.com".to_string()],
            ..Default::default()
        };
        assert!(acl.is_security("gregkh@linuxfoundation.org"));
        // A space in the configuration file must not let a denied address
        // through.
        assert!(acl.is_blocklisted("mallory@example.com"));
        assert!(acl.is_blocklisted(" mallory@example.com "));
    }

    #[test]
    fn test_acl_rejects_unknown_keys() {
        // The lists are the whole security model, so a typo has to be loud.
        let toml = r#"
            admins = ["alice@example.com"]
            securty = ["typo@example.com"]
        "#;
        assert!(toml::from_str::<AclSettings>(toml).is_err());
    }
}
