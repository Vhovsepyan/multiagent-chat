//! Bounded repository inspection for task-relevant agent context.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::technology::{ProjectProfile, detect};

const MAX_INSTRUCTION_BYTES: usize = 16 * 1024;
const INSTRUCTION_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "README.md"];
const METADATA_FILES: &[&str] = &[
    "Cargo.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "Dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryInstruction {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryInspection {
    pub profile: ProjectProfile,
    pub metadata: Vec<String>,
    pub instructions: Vec<RepositoryInstruction>,
}

impl RepositoryInspection {
    pub fn prompt_context(&self) -> String {
        let instructions = self
            .instructions
            .iter()
            .map(|item| format!("--- {} ---\n{}", item.path, item.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        format!(
            "Detected metadata: {}\n{}",
            if self.metadata.is_empty() {
                "none".into()
            } else {
                self.metadata.join(", ")
            },
            instructions
        )
    }
}

pub fn inspect(root: &Path) -> Result<RepositoryInspection> {
    let profile = detect(root)?;
    let metadata = METADATA_FILES
        .iter()
        .filter(|name| root.join(name).is_file())
        .map(|name| (*name).to_string())
        .collect();
    let instructions = INSTRUCTION_FILES
        .iter()
        .filter_map(|name| {
            let path = root.join(name);
            let bytes = fs::read(path).ok()?;
            let bounded = &bytes[..bytes.len().min(MAX_INSTRUCTION_BYTES)];
            Some(RepositoryInstruction {
                path: (*name).to_string(),
                content: String::from_utf8_lossy(bounded).into_owned(),
            })
        })
        .collect();
    Ok(RepositoryInspection {
        profile,
        metadata,
        instructions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn discovers_relevant_instructions_without_unrelated_files() {
        let root = std::env::temp_dir().join(format!("mac-inspect-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        fs::write(root.join("AGENTS.md"), "follow me").unwrap();
        fs::write(root.join("notes.txt"), "do not load").unwrap();
        let result = inspect(&root).unwrap();
        assert_eq!(result.metadata, vec!["Cargo.toml"]);
        assert_eq!(result.instructions.len(), 1);
        assert!(!result.prompt_context().contains("do not load"));
        fs::remove_dir_all(root).ok();
    }
}
