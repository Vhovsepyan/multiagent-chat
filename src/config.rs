//! Reads the `.env` file / environment into one `Config` struct.
//!
//! Everything the rest of the app needs to know about the outside world lives
//! here, so no other module has to touch `std::env` directly.

// The API-key fields are read starting in Phase 1.
#![allow(dead_code)]

use std::env;
use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

/// All settings for one run of the app.
#[derive(Clone)]
pub struct Config {
    pub execution: crate::execution_limits::ExecutionLimits,
    /// Credentials are per provider and independent: an installation may
    /// configure one chat provider, both, or neither (the Claude Code worker
    /// authenticates itself). A provider without its key is simply not offered.
    pub gemini_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub openai_api_key: Option<String>,
    pub openai_base_url: String,
    /// Folder that holds all of the user's projects. The repo for one run is
    /// chosen inside this folder at runtime — see `target.rs`.
    /// Optional compatibility setting for the original terminal workflow.
    /// Production web tasks use repository-backed temporary workspaces.
    pub workspace_root: Option<PathBuf>,
    /// The ONE folder a persistent New Project may be written into (task 0010).
    /// Falls back to `workspace_root`, which already means "the folder holding
    /// this user's projects". `None` means persistent output is refused.
    pub persistent_output_root: Option<PathBuf>,
    pub max_rounds: u32,
    /// How many worker fix iterations one milestone may use after the critic
    /// reviews the implementation (task 0011). Zero means the review still
    /// runs, but findings fail the milestone instead of being fixed.
    pub max_fix_iterations: u32,
    /// Default model for the Gemini provider.
    pub gemini_model: String,
    /// Default model for the Anthropic provider.
    pub critic_model: String,
    /// Model Claude Code runs the implementation with.
    pub implementer_model: String,
    /// Extra models offered per provider/tool in the task form (task 0005).
    /// The default above is always offered as well, so these lists only add.
    pub gemini_models: Vec<String>,
    pub anthropic_models: Vec<String>,
    pub openai_model: String,
    pub openai_models: Vec<String>,
    pub claude_code_models: Vec<String>,
    /// Models offered for the Codex worker tool.
    pub codex_models: Vec<String>,
    /// Default model for the Codex worker tool.
    pub codex_model: String,
    /// Permission mode passed to Claude Code. See `implementer.rs` for why the
    /// default is the permissive one.
    pub permission_mode: String,
    /// Port the v2 web server listens on (`--web`).
    pub port: u16,
}

/// Defaults used when the variable is missing from `.env`.
const DEFAULT_MAX_ROUNDS: u32 = 5;
const DEFAULT_MAX_FIX_ITERATIONS: u32 = 2;
/// The fix loop is meant to be short; a large value is a configuration mistake.
const MAX_FIX_ITERATION_LIMIT: u32 = 5;
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-3.6-flash";
pub const DEFAULT_CRITIC_MODEL: &str = "claude-sonnet-4-6";
pub const DEFAULT_IMPLEMENTER_MODEL: &str = "claude-opus-4-8";
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5.3-codex";
pub const DEFAULT_OPENAI_MODEL: &str = "gpt-5.6-sol";
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_OPENAI_MODELS: &[&str] = &[
    "gpt-6-astra",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
];
const DEFAULT_ANTHROPIC_MODELS: &[&str] = &[
    "claude-opus-5",
    "claude-sonnet-5",
    "claude-fable-5",
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251001",
];
const DEFAULT_GEMINI_MODELS: &[&str] = &[
    "gemini-3.8-flash",
    "gemini-3.7-flash",
    "gemini-3.6-flash",
    "gemini-3.5-flash",
    "gemini-3.5-flash-lite",
    "gemini-3.1-pro-preview",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
];
const DEFAULT_PERMISSION_MODE: &str = "bypassPermissions";
const DEFAULT_PORT: u16 = 3000;

impl Config {
    /// The credential for one chat provider, or `None` when this installation
    /// does not configure it. Provider availability is decided from this.
    pub fn chat_credential(&self, provider: crate::agent::ChatProvider) -> Option<&str> {
        match provider {
            crate::agent::ChatProvider::Gemini => self.gemini_api_key.as_deref(),
            crate::agent::ChatProvider::Anthropic => self.anthropic_api_key.as_deref(),
            crate::agent::ChatProvider::OpenAI => self.openai_api_key.as_deref(),
        }
    }

    /// Load `.env` (if present) and build a `Config`.
    ///
    /// Returns `Err` with a readable message if a required variable is missing
    /// or malformed, so `main` can print it and exit cleanly.
    pub fn load() -> Result<Self> {
        // Missing .env is fine — the variables may come from the real
        // environment instead. Any other error (unreadable file) is a problem.
        match dotenvy::dotenv() {
            Ok(_) => {}
            Err(e) if e.not_found() => {}
            Err(e) => {
                return Err(e).context(
                    "failed to read .env — a Windows path written with backslashes breaks \
                     the parser, because a backslash starts an escape sequence. Use \
                     forward slashes (C:/Users/you/repo), or wrap the value in single quotes.",
                );
            }
        }

        let workspace_root = env::var("WORKSPACE_ROOT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from);
        if let Some(path) = &workspace_root
            && !path.is_dir()
        {
            bail!(
                "WORKSPACE_ROOT does not point at an existing directory: {}",
                path.display()
            );
        }

        // Task 0010: only this folder may receive a persistent New Project, and
        // a configured one must already exist — the application never creates
        // an output root it was told about but cannot find.
        let persistent_output_root = env::var("PERSISTENT_OUTPUT_ROOT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from);
        if let Some(path) = &persistent_output_root
            && !path.is_dir()
        {
            bail!(
                "PERSISTENT_OUTPUT_ROOT does not point at an existing directory: {}",
                path.display()
            );
        }
        let persistent_output_root = persistent_output_root.or_else(|| workspace_root.clone());

        let max_rounds = match env::var("MAX_ROUNDS") {
            Ok(raw) => raw
                .trim()
                .parse::<u32>()
                .with_context(|| format!("MAX_ROUNDS must be a whole number, got {raw:?}"))?,
            Err(_) => DEFAULT_MAX_ROUNDS,
        };
        if max_rounds == 0 {
            bail!("MAX_ROUNDS must be at least 1");
        }

        // Task 0011: bounded by construction, so a critic that keeps asking
        // for changes can never hold a milestone open indefinitely.
        let max_fix_iterations = match env::var("MAX_FIX_ITERATIONS") {
            Ok(raw) => raw.trim().parse::<u32>().with_context(|| {
                format!("MAX_FIX_ITERATIONS must be a whole number, got {raw:?}")
            })?,
            Err(_) => DEFAULT_MAX_FIX_ITERATIONS,
        };
        if max_fix_iterations > MAX_FIX_ITERATION_LIMIT {
            bail!("MAX_FIX_ITERATIONS must be at most {MAX_FIX_ITERATION_LIMIT}");
        }

        let port = match env::var("PORT") {
            Ok(raw) => raw
                .trim()
                .parse::<u16>()
                .with_context(|| format!("PORT must be a number 1-65535, got {raw:?}"))?,
            Err(_) => DEFAULT_PORT,
        };

        Ok(Config {
            execution: crate::execution_limits::ExecutionLimits::load()?,
            gemini_api_key: credential("GEMINI_API_KEY"),
            anthropic_api_key: credential("ANTHROPIC_API_KEY"),
            openai_api_key: credential("OPENAI_API_KEY"),
            openai_base_url: optional("OPENAI_BASE_URL", DEFAULT_OPENAI_BASE_URL)
                .trim_end_matches('/')
                .to_string(),
            workspace_root,
            persistent_output_root,
            max_rounds,
            max_fix_iterations,
            gemini_model: optional("GEMINI_MODEL", DEFAULT_GEMINI_MODEL),
            critic_model: optional("CRITIC_MODEL", DEFAULT_CRITIC_MODEL),
            implementer_model: optional("IMPLEMENTER_MODEL", DEFAULT_IMPLEMENTER_MODEL),
            gemini_models: model_list("GEMINI_MODELS", DEFAULT_GEMINI_MODELS),
            anthropic_models: model_list("ANTHROPIC_MODELS", DEFAULT_ANTHROPIC_MODELS),
            openai_model: optional("OPENAI_MODEL", DEFAULT_OPENAI_MODEL),
            openai_models: model_list("OPENAI_MODELS", DEFAULT_OPENAI_MODELS),
            claude_code_models: model_list("CLAUDE_CODE_MODELS", &[]),
            codex_models: model_list("CODEX_MODELS", &[]),
            codex_model: optional("CODEX_MODEL", DEFAULT_CODEX_MODEL),
            permission_mode: optional("CLAUDE_PERMISSION_MODE", DEFAULT_PERMISSION_MODE),
            port,
        })
    }
}

/// A comma-separated list of model names, e.g. `GEMINI_MODELS=a,b,c`.
///
/// Blank entries are dropped rather than becoming an unselectable empty model.
/// Built-in catalog entries are retained, and an environment variable may add
/// further entries without a code change.
fn model_list(name: &str, defaults: &[&str]) -> Vec<String> {
    let raw = env::var(name).unwrap_or_default();
    let mut models: Vec<String> = defaults.iter().map(|model| (*model).to_string()).collect();
    for model in raw.split(',') {
        let model = model.trim();
        if !model.is_empty() && !models.iter().any(|known| known == model) {
            models.push(model.to_string());
        }
    }
    models
}

/// A provider credential. Missing or blank means "this provider is not
/// configured", which is a supported state — the catalogue then does not offer
/// it, and a task that asks for it is refused with a clear message. Startup
/// does not require any particular key.
fn credential(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// A variable with a sensible fallback.
fn optional(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

/// Says whether a credential is configured without revealing any of it.
fn redacted(value: &Option<String>) -> &'static str {
    match value {
        Some(_) => "<redacted>",
        None => "<unset>",
    }
}

/// Hand-written `Debug` so that printing a `Config` can never leak a key.
///
/// (If we had used `#[derive(Debug)]`, `println!("{config:?}")` would dump the
/// raw API keys into the terminal and into any log file.)
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("execution", &self.execution)
            .field("gemini_api_key", &redacted(&self.gemini_api_key))
            .field("anthropic_api_key", &redacted(&self.anthropic_api_key))
            .field("openai_api_key", &redacted(&self.openai_api_key))
            .field("workspace_root", &self.workspace_root)
            .field("persistent_output_root", &self.persistent_output_root)
            .field("max_rounds", &self.max_rounds)
            .field("max_fix_iterations", &self.max_fix_iterations)
            .field("gemini_model", &self.gemini_model)
            .field("critic_model", &self.critic_model)
            .field("openai_model", &self.openai_model)
            .field("implementer_model", &self.implementer_model)
            .field("codex_model", &self.codex_model)
            .field("permission_mode", &self.permission_mode)
            .field("port", &self.port)
            .finish()
    }
}
