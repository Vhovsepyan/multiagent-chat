//! Colored terminal output.
//!
//! One place for every color decision, so the debate loop can just say
//! `ui::proposer("...")` and not care about escape codes.

// Phase 0 only calls a few of these; the rest are used from Phase 3 on.
#![allow(dead_code)]

use owo_colors::OwoColorize;

/// Proposer (Gemini) — blue.
pub fn proposer(body: &str) {
    println!("{}", "── PROPOSER (Gemini) ─────────────────".blue().bold());
    println!("{}", body.blue());
}

/// Critic (Claude) — yellow/orange.
pub fn critic(body: &str) {
    println!(
        "{}",
        "── CRITIC (Claude) ───────────────────".yellow().bold()
    );
    println!("{}", body.yellow());
}

/// System / progress messages — gray.
pub fn system(msg: &str) {
    println!("{}", msg.bright_black());
}

/// A section heading, e.g. the round number.
pub fn header(msg: &str) {
    println!("\n{}", msg.bold());
}

/// Something the user must notice but that is not fatal.
pub fn warn(msg: &str) {
    println!("{} {}", "warning:".yellow().bold(), msg);
}

/// A fatal problem.
pub fn error(msg: &str) {
    eprintln!("{} {}", "error:".red().bold(), msg);
}

/// Success / approval.
pub fn success(msg: &str) {
    println!("{}", msg.green().bold());
}
