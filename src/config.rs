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
    pub gemini_api_key: String,
    pub anthropic_api_key: String,
    pub target_repo_path: PathBuf,
    pub max_rounds: u32,
    pub gemini_model: String,
    pub critic_model: String,
}

/// Defaults used when the variable is missing from `.env`.
const DEFAULT_MAX_ROUNDS: u32 = 5;
const DEFAULT_GEMINI_MODEL: &str = "gemini-3.7-flash";
const DEFAULT_CRITIC_MODEL: &str = "claude-sonnet-4-6";

impl Config {
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
            Err(e) => return Err(e).context("failed to read .env"),
        }

        let target_repo_path = PathBuf::from(required("TARGET_REPO_PATH")?);
        if !target_repo_path.is_dir() {
            bail!(
                "TARGET_REPO_PATH does not point at an existing directory: {}",
                target_repo_path.display()
            );
        }

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

        Ok(Config {
            gemini_api_key: required("GEMINI_API_KEY")?,
            anthropic_api_key: required("ANTHROPIC_API_KEY")?,
            target_repo_path,
            max_rounds,
            gemini_model: optional("GEMINI_MODEL", DEFAULT_GEMINI_MODEL),
            critic_model: optional("CRITIC_MODEL", DEFAULT_CRITIC_MODEL),
        })
    }
}

/// A variable the app cannot run without.
fn required(name: &str) -> Result<String> {
    let value = env::var(name)
        .with_context(|| format!("{name} is not set — copy .env.example to .env and fill it in"))?;
    if value.trim().is_empty() {
        bail!("{name} is set but empty");
    }
    Ok(value)
}

/// A variable with a sensible fallback.
fn optional(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

/// Hand-written `Debug` so that printing a `Config` can never leak a key.
///
/// (If we had used `#[derive(Debug)]`, `println!("{config:?}")` would dump the
/// raw API keys into the terminal and into any log file.)
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("gemini_api_key", &"<redacted>")
            .field("anthropic_api_key", &"<redacted>")
            .field("target_repo_path", &self.target_repo_path)
            .field("max_rounds", &self.max_rounds)
            .field("gemini_model", &self.gemini_model)
            .field("critic_model", &self.critic_model)
            .finish()
    }
}
