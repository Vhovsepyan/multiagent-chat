//! Regression fixtures for repository-controlled filesystem links.

use crate::inspection::{InspectionRequest, inspect};
use crate::task::TaskKind;
use std::fs;
use std::path::{Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("mac-safety-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(path.join("repo")).unwrap();
        fs::write(
            path.join("outside.txt"),
            "private sentinel spring-boot typescript",
        )
        .unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn file_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, link).unwrap();
}

#[test]
#[cfg_attr(
    windows,
    ignore = "requires Windows Developer Mode or symlink privilege; runs normally on Unix"
)]
fn inspection_never_reads_linked_instructions_or_metadata() {
    let fixture = Fixture::new();
    let root = fixture.0.join("repo");
    for name in [
        "AGENTS.md",
        "CLAUDE.md",
        "README.md",
        "pom.xml",
        "package.json",
    ] {
        file_link(&fixture.0.join("outside.txt"), &root.join(name));
    }
    let inspection = inspect(
        &root,
        InspectionRequest {
            kind: TaskKind::BugFix,
            title: "private sentinel",
            description: "check repository",
        },
    )
    .unwrap();
    assert!(inspection.instructions.is_empty());
    assert!(inspection.metadata.is_empty());
    assert!(!inspection.prompt_context().contains("private sentinel"));
    assert!(inspection.profile.framework.is_none());
}

#[test]
#[cfg_attr(
    windows,
    ignore = "requires Windows Developer Mode or symlink privilege; runs normally on Unix"
)]
fn artifact_write_refuses_external_and_dangling_links() {
    for dangling in [false, true] {
        let fixture = Fixture::new();
        let root = fixture.0.join("artifacts");
        fs::create_dir(&root).unwrap();
        let outside = fixture.0.join(if dangling {
            "missing.txt"
        } else {
            "outside.txt"
        });
        file_link(&outside, &root.join(crate::spec::APPROVED_SPEC_FILENAME));
        assert!(crate::spec::write_artifact(&root, "replacement").is_err());
        assert!(
            fs::symlink_metadata(root.join(crate::spec::APPROVED_SPEC_FILENAME))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        if dangling {
            assert!(!outside.exists());
        } else {
            assert_eq!(
                fs::read_to_string(outside).unwrap(),
                "private sentinel spring-boot typescript"
            );
        }
    }
}

#[test]
fn artifact_replacement_does_not_truncate_hard_link_targets() {
    let fixture = Fixture::new();
    let root = fixture.0.join("artifacts");
    fs::create_dir(&root).unwrap();
    fs::hard_link(
        fixture.0.join("outside.txt"),
        root.join(crate::spec::APPROVED_SPEC_FILENAME),
    )
    .unwrap();
    crate::spec::write_artifact(&root, "approved").unwrap();
    assert_eq!(
        fs::read_to_string(root.join(crate::spec::APPROVED_SPEC_FILENAME)).unwrap(),
        "approved"
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("outside.txt")).unwrap(),
        "private sentinel spring-boot typescript"
    );
}

#[test]
#[cfg_attr(
    windows,
    ignore = "requires Windows Developer Mode or symlink privilege; runs normally on Unix"
)]
fn repository_reads_reject_linked_parent_directories() {
    let fixture = Fixture::new();
    let root = fixture.0.join("repo");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&fixture.0, root.join("linked")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&fixture.0, root.join("linked")).unwrap();
    assert!(crate::repository_file::open(&root, &root.join("linked/outside.txt")).is_err());
}
