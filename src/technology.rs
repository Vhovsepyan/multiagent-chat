//! Evidence-based project technology detection.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TechStack {
    Rust,
    JavaSpringBoot,
    Python,
    TypeScriptNode,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildTool {
    Cargo,
    Maven,
    Gradle,
    Npm,
    Python,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectProfile {
    pub stack: TechStack,
    pub build_tool: BuildTool,
    pub framework: Option<String>,
    pub evidence: Vec<String>,
}

impl ProjectProfile {
    pub fn selected(stack: TechStack) -> Self {
        let (build_tool, framework) = match stack {
            TechStack::Rust => (BuildTool::Cargo, None),
            TechStack::JavaSpringBoot => (BuildTool::Gradle, Some("Spring Boot".into())),
            TechStack::Python => (BuildTool::Python, None),
            TechStack::TypeScriptNode => (BuildTool::Npm, Some("Node.js".into())),
            TechStack::Custom => (BuildTool::Custom, None),
        };
        Self {
            stack,
            build_tool,
            framework,
            evidence: vec!["selected by user".into()],
        }
    }
}

pub fn detect(root: &Path) -> Result<ProjectProfile> {
    if root.join("Cargo.toml").is_file() {
        return Ok(profile(
            TechStack::Rust,
            BuildTool::Cargo,
            None,
            &["Cargo.toml"],
        ));
    }

    if root.join("pom.xml").is_file() {
        let pom = read_lossy(&root.join("pom.xml"));
        return Ok(profile(
            TechStack::JavaSpringBoot,
            BuildTool::Maven,
            pom.contains("spring-boot").then(|| "Spring Boot".into()),
            &["pom.xml"],
        ));
    }

    let gradle = ["build.gradle.kts", "build.gradle"]
        .into_iter()
        .find(|name| root.join(name).is_file());
    if let Some(name) = gradle {
        let build = read_lossy(&root.join(name));
        return Ok(profile(
            TechStack::JavaSpringBoot,
            BuildTool::Gradle,
            build.contains("spring-boot").then(|| "Spring Boot".into()),
            &[name],
        ));
    }

    if root.join("package.json").is_file() {
        let package = read_lossy(&root.join("package.json"));
        let typescript = root.join("tsconfig.json").is_file() || package.contains("typescript");
        let mut evidence = vec!["package.json".to_string()];
        if root.join("tsconfig.json").is_file() {
            evidence.push("tsconfig.json".into());
        }
        return Ok(ProjectProfile {
            stack: TechStack::TypeScriptNode,
            build_tool: BuildTool::Npm,
            framework: Some(if typescript {
                "Node.js / TypeScript".into()
            } else {
                "Node.js / JavaScript".into()
            }),
            evidence,
        });
    }

    if root.join("pyproject.toml").is_file() || root.join("requirements.txt").is_file() {
        let evidence = ["pyproject.toml", "requirements.txt"]
            .into_iter()
            .filter(|name| root.join(name).is_file())
            .map(str::to_string)
            .collect();
        return Ok(ProjectProfile {
            stack: TechStack::Python,
            build_tool: BuildTool::Python,
            framework: None,
            evidence,
        });
    }

    Ok(profile(TechStack::Custom, BuildTool::Custom, None, &[]))
}

fn profile(
    stack: TechStack,
    build_tool: BuildTool,
    framework: Option<String>,
    evidence: &[&str],
) -> ProjectProfile {
    ProjectProfile {
        stack,
        build_tool,
        framework,
        evidence: evidence.iter().map(|value| (*value).to_string()).collect(),
    }
}

fn read_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn fixture(files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mac-tech-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        for (name, body) in files {
            fs::write(root.join(name), body).unwrap();
        }
        root
    }

    fn detected(files: &[(&str, &str)]) -> ProjectProfile {
        let root = fixture(files);
        let result = detect(&root).unwrap();
        fs::remove_dir_all(root).ok();
        result
    }

    #[test]
    fn detects_supported_stacks() {
        assert_eq!(detected(&[("Cargo.toml", "")]).stack, TechStack::Rust);
        assert_eq!(
            detected(&[("pom.xml", "spring-boot")]).build_tool,
            BuildTool::Maven
        );
        assert_eq!(
            detected(&[("build.gradle.kts", "spring-boot")]).build_tool,
            BuildTool::Gradle
        );
        assert_eq!(
            detected(&[("package.json", "{}")]).stack,
            TechStack::TypeScriptNode
        );
        assert_eq!(detected(&[("pyproject.toml", "")]).stack, TechStack::Python);
        assert_eq!(detected(&[]).stack, TechStack::Custom);
    }
}
