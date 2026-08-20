//! Colored terminal output.
//!
//! One place for every color decision, so the debate loop can just say
//! `ui::proposer("...")` and not care about escape codes.

// Phase 0 only calls a few of these; the rest are used from Phase 3 on.
#![allow(dead_code)]

use std::io::{self, Write};

use anyhow::{Result, bail};
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

/// Ask for a line of text. Pressing Enter accepts `default`.
///
/// `&str` is a borrowed view of a string — we only read it here, so there is no
/// need to take ownership of the caller's `String`.
pub fn prompt(label: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{}: ", label.bold());
    } else {
        print!("{} [{}]: ", label.bold(), default.bright_black());
    }
    io::stdout().flush()?;

    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        bail!("input ended before {label} was given");
    }

    let answer = line.trim();
    if answer.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(answer.to_string())
    }
}

/// Ask a yes/no question. Anything other than y/yes counts as no.
pub fn confirm(question: &str) -> Result<bool> {
    let answer = prompt(&format!("{question} [y/n]"), "")?;
    Ok(matches!(answer.to_lowercase().as_str(), "y" | "yes"))
}
