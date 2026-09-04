//! Technology-aware verification planning and execution.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::technology::{BuildTool, ProjectProfile};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl VerificationCommand {
    fn new(program: impl Into<String>, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
        }
    }

    pub fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub command: String,
    pub success: bool,
    pub output: String,
}

pub fn plan(profile: &ProjectProfile, root: &Path) -> Vec<VerificationCommand> {
    match profile.build_tool {
        BuildTool::Cargo => vec![
            VerificationCommand::new("cargo", &["fmt", "--check"]),
            VerificationCommand::new(
                "cargo",
                &[
                    "clippy",
                    "--all-targets",
                    "--all-features",
                    "--",
                    "-D",
                    "warnings",
                ],
            ),
            VerificationCommand::new("cargo", &["test"]),
        ],
        BuildTool::Maven => vec![VerificationCommand::new(
            wrapper(root, "mvnw", "mvnw.cmd", "mvn"),
            &["test"],
        )],
        BuildTool::Gradle => vec![VerificationCommand::new(
            wrapper(root, "gradlew", "gradlew.bat", "gradle"),
            &["test"],
        )],
        BuildTool::Npm => node_commands(root),
        BuildTool::Python => {
            if root.join("pyproject.toml").is_file()
                || root.join("pytest.ini").is_file()
                || root.join("tests").is_dir()
            {
                vec![VerificationCommand::new("pytest", &[])]
            } else {
                Vec::new()
            }
        }
        BuildTool::Custom => Vec::new(),
    }
}

fn wrapper(root: &Path, unix: &str, windows: &str, fallback: &str) -> String {
    if cfg!(windows) && root.join(windows).is_file() {
        format!(".\\{windows}")
    } else if root.join(unix).is_file() {
        format!("./{unix}")
    } else {
        fallback.to_string()
    }
}

fn node_commands(root: &Path) -> Vec<VerificationCommand> {
    let scripts = std::fs::read_to_string(root.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("scripts").cloned())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    ["test", "lint", "typecheck", "build"]
        .into_iter()
        .filter(|name| scripts.contains_key(*name))
        .map(|name| VerificationCommand::new("npm", &["run", name]))
        .collect()
}

pub async fn run(commands: &[VerificationCommand], root: &Path) -> Result<Vec<VerificationResult>> {
    let mut results = Vec::new();
    for command in commands {
        let output = Command::new(&command.program)
            .args(&command.args)
            .current_dir(root)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("could not run {}", command.display()))?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        results.push(VerificationResult {
            command: command.display(),
            success: output.status.success(),
            output: combined.chars().take(16_000).collect(),
        });
        if !output.status.success() {
            break;
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::technology::TechStack;
    use uuid::Uuid;

    #[test]
    fn selects_commands_by_profile_and_repository_scripts() {
        let root = std::env::temp_dir().join(format!("mac-verify-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let rust = plan(&ProjectProfile::selected(TechStack::Rust), &root);
        assert_eq!(rust.len(), 3);
        assert!(rust[1].display().contains("clippy"));

        std::fs::write(
            root.join("package.json"),
            r#"{"scripts":{"test":"x","build":"x"}}"#,
        )
        .unwrap();
        let node = plan(&ProjectProfile::selected(TechStack::TypeScriptNode), &root);
        assert_eq!(node.len(), 2);
        assert!(
            node.iter()
                .all(|command| !command.display().contains("lint"))
        );
        std::fs::remove_dir_all(root).ok();
    }
}
