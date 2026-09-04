//! Repository-backed projects and their in-memory persistence boundary.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::technology::ProjectProfile;

pub type ProjectId = Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ProjectSource {
    #[serde(rename = "github")]
    GitHub { repository: String },
}

impl ProjectSource {
    pub fn github(repository: &str) -> Result<Self> {
        Ok(Self::GitHub {
            repository: normalize_github_repository(repository)?,
        })
    }

    pub fn clone_url(&self) -> String {
        match self {
            Self::GitHub { repository } => format!("https://github.com/{repository}.git"),
        }
    }
}

fn normalize_github_repository(raw: &str) -> Result<String> {
    let mut value = raw.trim().trim_end_matches('/').trim_end_matches(".git");
    if let Some(rest) = value.strip_prefix("https://github.com/") {
        value = rest;
    } else if let Some(rest) = value.strip_prefix("http://github.com/") {
        value = rest;
    }
    if value.contains(['?', '#', '@']) {
        bail!("GitHub repository must not contain credentials, query strings, or fragments");
    }
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != 2
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(valid_repo_char))
    {
        bail!("GitHub repository must be owner/repository or a github.com HTTPS URL");
    }
    Ok(format!("{}/{}", parts[0], parts[1]))
}

fn valid_repo_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.')
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub source: ProjectSource,
    pub default_branch: String,
    pub detected_stack: Option<ProjectProfile>,
}

impl Project {
    pub fn new(name: &str, source: ProjectSource, default_branch: &str) -> Result<Self> {
        let name = name.trim();
        let default_branch = default_branch.trim();
        if name.is_empty() {
            bail!("project name cannot be empty");
        }
        if default_branch.is_empty() {
            bail!("default branch cannot be empty");
        }
        Ok(Self {
            id: Uuid::new_v4(),
            name: name.to_string(),
            source,
            default_branch: default_branch.to_string(),
            detected_stack: None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProjectStore {
    projects: Arc<RwLock<HashMap<ProjectId, Project>>>,
}

impl ProjectStore {
    pub fn register(&self, project: Project) -> Result<Project> {
        let mut projects = self.projects.write().expect("project store lock poisoned");
        if projects.values().any(|existing| {
            existing.name.eq_ignore_ascii_case(&project.name) || existing.source == project.source
        }) {
            bail!("a project with that name or repository is already registered");
        }
        projects.insert(project.id, project.clone());
        Ok(project)
    }

    pub fn get(&self, id: ProjectId) -> Option<Project> {
        self.projects
            .read()
            .expect("project store lock poisoned")
            .get(&id)
            .cloned()
    }

    pub fn list(&self) -> Vec<Project> {
        let mut projects: Vec<_> = self
            .projects
            .read()
            .expect("project store lock poisoned")
            .values()
            .cloned()
            .collect();
        projects.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        projects
    }

    pub fn set_profile(&self, id: ProjectId, profile: ProjectProfile) -> bool {
        let mut projects = self.projects.write().expect("project store lock poisoned");
        match projects.get_mut(&id) {
            Some(project) => {
                project.detected_stack = Some(profile);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_sources_are_normalized_and_reject_credentials() {
        assert_eq!(
            ProjectSource::github("https://github.com/openai/example.git/").unwrap(),
            ProjectSource::GitHub {
                repository: "openai/example".into()
            }
        );
        assert!(ProjectSource::github("https://token@github.com/o/r").is_err());
        assert!(ProjectSource::github("not-a-repository").is_err());
    }

    #[test]
    fn projects_can_be_registered_and_updated() {
        let store = ProjectStore::default();
        let project = Project::new(
            "Example",
            ProjectSource::github("openai/example").unwrap(),
            "main",
        )
        .unwrap();
        store.register(project.clone()).unwrap();
        assert_eq!(store.get(project.id).unwrap().name, "Example");
        assert!(store.register(project.clone()).is_err());
    }
}
