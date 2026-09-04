//! Bounded, task-aware repository inspection for agent context.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::task::TaskKind;
use crate::technology::{ProjectProfile, detect};

const MAX_TREE_ENTRIES: usize = 400;
const MAX_WALKED_FILES: usize = 2_400;
const MAX_CANDIDATE_FILES: usize = 2_000;
const MAX_SCORE_BYTES: usize = 8 * 1024;
const MAX_FILE_BYTES: usize = 16 * 1024;
const MAX_SOURCE_FILES: usize = 10;
const MAX_TEST_FILES: usize = 6;
pub(crate) const MAX_PROMPT_CONTEXT_BYTES: usize = 96 * 1024;
const MAX_METADATA_BYTES: usize = 12 * 1024;
const MAX_INSTRUCTION_BYTES: usize = 18 * 1024;
const MAX_SOURCE_BYTES: usize = 36 * 1024;
const MAX_TEST_BYTES: usize = 16 * 1024;

const INSTRUCTION_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md", "README.md"];
const METADATA_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "Dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
];
const EXCLUDED_DIRECTORIES: &[&str] = &[
    ".git",
    ".idea",
    ".next",
    "build",
    "coverage",
    "dist",
    "generated",
    "node_modules",
    "out",
    "target",
    "vendor",
];
const SOURCE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "go", "h", "hpp", "java", "js", "jsx", "kt", "kts", "php", "py", "rb",
    "rs", "scala", "sql", "swift", "ts", "tsx", "vue",
];
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "bug", "change", "fix", "for", "in", "of", "on", "the", "to", "with",
];

#[derive(Debug, Clone, Copy)]
pub struct InspectionRequest<'a> {
    pub kind: TaskKind,
    pub title: &'a str,
    pub description: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectedFile {
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryInspection {
    pub profile: ProjectProfile,
    pub tree: Vec<String>,
    pub metadata: Vec<InspectedFile>,
    pub instructions: Vec<InspectedFile>,
    pub relevant_sources: Vec<InspectedFile>,
    pub relevant_tests: Vec<InspectedFile>,
}

impl RepositoryInspection {
    pub fn prompt_context(&self) -> String {
        let mut context = String::new();
        push_bounded(
            &mut context,
            &format!("## Detected technology\n{:?}\n\n", self.profile),
        );
        push_bounded(
            &mut context,
            &format!("## Repository tree\n{}\n\n", self.tree.join("\n")),
        );
        append_files(&mut context, "Build and project metadata", &self.metadata);
        append_files(&mut context, "Repository instructions", &self.instructions);
        append_files(
            &mut context,
            "Task-relevant source files",
            &self.relevant_sources,
        );
        append_files(&mut context, "Relevant tests", &self.relevant_tests);
        context
    }
}

pub fn requires_repository_inspection(kind: TaskKind) -> bool {
    matches!(kind, TaskKind::Feature | TaskKind::BugFix)
}

pub fn inspect(root: &Path, request: InspectionRequest<'_>) -> Result<RepositoryInspection> {
    if !requires_repository_inspection(request.kind) {
        bail!("new-project tasks do not inspect an existing repository");
    }

    let profile = detect(root)?;
    let mut paths = Vec::new();
    walk(root, &mut paths)?;
    paths.sort();
    let tree = paths
        .iter()
        .take(MAX_TREE_ENTRIES)
        .map(|path| relative(root, path))
        .collect();
    let keywords = task_keywords(request.title, request.description);
    let mut candidates = paths
        .iter()
        .filter(|path| is_source(path) || is_test(path))
        .take(MAX_CANDIDATE_FILES)
        .filter_map(|path| {
            let preview = read_prefix(path, MAX_SCORE_BYTES).ok()?;
            let score = relevance_score(root, path, &preview, &keywords, request.kind);
            (score > 0).then(|| Candidate {
                path: path.clone(),
                score,
                test: is_test(path),
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| {
        (
            Reverse(candidate.score),
            relative(root, &candidate.path).to_lowercase(),
        )
    });

    let mut metadata_budget = MAX_METADATA_BYTES;
    let mut instruction_budget = MAX_INSTRUCTION_BYTES;
    let mut source_budget = MAX_SOURCE_BYTES;
    let mut test_budget = MAX_TEST_BYTES;
    let metadata = read_named_files(root, METADATA_FILES, &mut metadata_budget);
    let instructions = read_named_files(root, INSTRUCTION_FILES, &mut instruction_budget);
    let relevant_sources = read_candidates(
        root,
        candidates.iter().filter(|item| !item.test),
        MAX_SOURCE_FILES,
        &mut source_budget,
    );
    let relevant_tests = read_candidates(
        root,
        candidates.iter().filter(|item| item.test),
        MAX_TEST_FILES,
        &mut test_budget,
    );

    Ok(RepositoryInspection {
        profile,
        tree,
        metadata,
        instructions,
        relevant_sources,
        relevant_tests,
    })
}

#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    score: usize,
    test: bool,
}

fn walk(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    if paths.len() >= MAX_WALKED_FILES {
        return Ok(());
    }
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("could not inspect {}", directory.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if !is_excluded_directory(&entry.file_name().to_string_lossy()) {
                walk(&path, paths)?;
            }
        } else if file_type.is_file() {
            paths.push(path);
        }
        if paths.len() >= MAX_WALKED_FILES {
            break;
        }
    }
    Ok(())
}

fn is_excluded_directory(name: &str) -> bool {
    EXCLUDED_DIRECTORIES
        .iter()
        .any(|excluded| name.eq_ignore_ascii_case(excluded))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SOURCE_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn is_test(path: &Path) -> bool {
    let lower = path.to_string_lossy().to_lowercase().replace('\\', "/");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_lowercase();
    lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/spec/")
        || lower.contains("/specs/")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("test.java")
        || name.ends_with("tests.rs")
}

fn task_keywords(title: &str, description: &str) -> Vec<String> {
    let mut keywords = HashSet::new();
    for value in [title, description] {
        let expanded = split_camel_case(value);
        for token in expanded
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|token| token.len() >= 3)
            .map(str::to_lowercase)
        {
            if !STOP_WORDS.contains(&token.as_str()) {
                keywords.insert(token);
            }
        }
    }
    let mut keywords = keywords.into_iter().collect::<Vec<_>>();
    keywords.sort();
    keywords
}

fn split_camel_case(value: &str) -> String {
    let mut expanded = String::with_capacity(value.len() * 2);
    let mut previous_lowercase = false;
    for character in value.chars() {
        if previous_lowercase && character.is_ascii_uppercase() {
            expanded.push(' ');
        }
        expanded.push(character);
        previous_lowercase = character.is_ascii_lowercase() || character.is_ascii_digit();
    }
    expanded
}

fn relevance_score(
    root: &Path,
    path: &Path,
    preview: &str,
    keywords: &[String],
    kind: TaskKind,
) -> usize {
    let relative = relative(root, path).to_lowercase();
    let content = preview.to_lowercase();
    let mut score = 0;
    for keyword in keywords {
        if relative.contains(keyword) {
            score += 30;
        }
        if content.contains(keyword) {
            score += 8;
        }
    }
    if relative.contains("/src/") || relative.starts_with("src/") {
        score += 3;
    }
    if is_test(path) {
        score += if kind == TaskKind::BugFix { 10 } else { 5 };
    }
    score
}

fn read_prefix(path: &Path, limit: usize) -> Result<String> {
    let mut bytes = Vec::with_capacity(limit);
    fs::File::open(path)?
        .take(limit as u64)
        .read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_named_files(root: &Path, names: &[&str], remaining: &mut usize) -> Vec<InspectedFile> {
    names
        .iter()
        .filter_map(|name| {
            let path = root.join(name);
            if path.is_file() {
                read_inspected_file(root, &path, remaining).ok()
            } else {
                None
            }
        })
        .collect()
}

fn read_candidates<'a>(
    root: &Path,
    candidates: impl Iterator<Item = &'a Candidate>,
    limit: usize,
    remaining: &mut usize,
) -> Vec<InspectedFile> {
    candidates
        .take(limit)
        .filter_map(|candidate| read_inspected_file(root, &candidate.path, remaining).ok())
        .collect()
}

fn read_inspected_file(root: &Path, path: &Path, remaining: &mut usize) -> Result<InspectedFile> {
    if *remaining == 0 {
        bail!("inspection context budget exhausted");
    }
    let file = fs::File::open(path)?;
    let file_size = file.metadata()?.len() as usize;
    let limit = file_size.min(MAX_FILE_BYTES).min(*remaining);
    let mut bytes = Vec::with_capacity(limit);
    file.take(limit as u64).read_to_end(&mut bytes)?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    *remaining = remaining.saturating_sub(content.len());
    Ok(InspectedFile {
        path: relative(root, path),
        content,
        truncated: limit < file_size,
    })
}

fn append_files(context: &mut String, heading: &str, files: &[InspectedFile]) {
    push_bounded(context, &format!("## {heading}\n"));
    if files.is_empty() {
        push_bounded(context, "None found.\n\n");
        return;
    }
    for file in files {
        let marker = if file.truncated { " (truncated)" } else { "" };
        push_bounded(
            context,
            &format!("### {}{}\n{}\n\n", file.path, marker, file.content),
        );
    }
}

fn push_bounded(target: &mut String, value: &str) {
    let remaining = MAX_PROMPT_CONTEXT_BYTES.saturating_sub(target.len());
    if remaining == 0 {
        return;
    }
    if value.len() <= remaining {
        target.push_str(value);
        return;
    }
    let mut boundary = remaining;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    target.push_str(&value[..boundary]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("mac-inspect-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("src/payments")).unwrap();
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nname='payments'").unwrap();
        fs::write(root.join("AGENTS.md"), "preserve transaction semantics").unwrap();
        fs::write(
            root.join("src/payments/payment_service.rs"),
            "pub fn process_payment() { charge(); }",
        )
        .unwrap();
        fs::write(
            root.join("src/payments/payment_repository.rs"),
            "pub fn store_payment() {}",
        )
        .unwrap();
        fs::write(
            root.join("src/payments/caller.rs"),
            "fn checkout() { process_payment(); }",
        )
        .unwrap();
        fs::write(
            root.join("tests/payment_service_test.rs"),
            "fn duplicate_payment_is_rejected() {}",
        )
        .unwrap();
        root
    }

    fn request(kind: TaskKind) -> InspectionRequest<'static> {
        InspectionRequest {
            kind,
            title: "Fix duplicate payment processing in PaymentService",
            description: "Prevent duplicate charges and preserve payment repository behavior",
        }
    }

    #[test]
    fn selects_task_relevant_files_and_related_content() {
        let root = repository();
        let result = inspect(&root, request(TaskKind::Feature)).unwrap();
        let paths = result
            .relevant_sources
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&"src/payments/payment_service.rs"));
        assert!(paths.contains(&"src/payments/payment_repository.rs"));
        assert!(paths.contains(&"src/payments/caller.rs"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn excludes_build_vendor_and_generated_directories() {
        let root = repository();
        for directory in ["target", "node_modules", "build", "dist", ".idea"] {
            fs::create_dir_all(root.join(directory)).unwrap();
            fs::write(root.join(directory).join("payment_service.rs"), "payment").unwrap();
        }
        let context = inspect(&root, request(TaskKind::Feature))
            .unwrap()
            .prompt_context();
        for directory in ["target", "node_modules", "build", "dist", ".idea"] {
            assert!(!context.contains(&format!("{directory}/payment_service.rs")));
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn bounds_each_file_and_the_total_prompt_context() {
        let root = repository();
        fs::write(
            root.join("src/payments/large_payment_service.rs"),
            "payment ".repeat(MAX_PROMPT_CONTEXT_BYTES),
        )
        .unwrap();
        let result = inspect(&root, request(TaskKind::Feature)).unwrap();
        assert!(result.relevant_sources.iter().any(|file| file.truncated));
        assert!(result.prompt_context().len() <= MAX_PROMPT_CONTEXT_BYTES);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn feature_context_contains_relevant_source() {
        let root = repository();
        let context = inspect(&root, request(TaskKind::Feature))
            .unwrap()
            .prompt_context();
        assert!(context.contains("## Task-relevant source files"));
        assert!(context.contains("payment_service.rs"));
        assert!(context.contains("process_payment"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn bug_fix_context_contains_relevant_source_and_test() {
        let root = repository();
        let context = inspect(&root, request(TaskKind::BugFix))
            .unwrap()
            .prompt_context();
        assert!(context.contains("src/payments/payment_service.rs"));
        assert!(context.contains("tests/payment_service_test.rs"));
        assert!(context.contains("duplicate_payment_is_rejected"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn new_projects_do_not_require_repository_inspection() {
        assert!(!requires_repository_inspection(TaskKind::NewProject));
        assert!(requires_repository_inspection(TaskKind::Feature));
        assert!(requires_repository_inspection(TaskKind::BugFix));
    }
}
