//! Repository-backed orchestration in a disposable task workspace.

use anyhow::{Result, bail};

use crate::agent::CodingTaskRequest;
use crate::inspection::{InspectionRequest, inspect};
use crate::project::Project;
use crate::spec;
use crate::task::{Emitter, TaskEvent, TaskId, TaskKind, TaskManager, TaskResult, TaskStatus};
use crate::technology::ProjectProfile;
use crate::web::AppState;
use crate::workspace::{TaskWorkspace, WorkspaceRequest, diff_result_with_limits};

pub fn spawn(state: AppState, id: TaskId) {
    tokio::spawn(async move {
        let emitter = state.manager.emitter(id);
        let mut workspace = None;
        let result = run(&state, id, &emitter, &mut workspace).await;

        let report = emitter.clone();
        if let Err(error) = tokio::task::spawn_blocking(move || {
            finish_run(&state, id, &emitter, workspace.as_ref(), result);
        })
        .await
        {
            report.emit(TaskEvent::Finished {
                status: TaskStatus::Failed,
                error: Some(format!(
                    "task finalization failed: {error}; manual workspace recovery may be required"
                )),
            });
        }
    });
}

fn finish_run(
    state: &AppState,
    id: TaskId,
    emitter: &Emitter,
    workspace: Option<&TaskWorkspace>,
    result: Result<()>,
) {
    let mut may_cleanup = true;
    if result.is_err()
        && let Some(workspace) = workspace
        && state
            .manager
            .get(id)
            .is_some_and(|task| task.result.is_none())
    {
        match diff_result_with_limits(&workspace.path, &state.config.execution) {
            Ok(diff) => {
                let verification = state
                    .manager
                    .get(id)
                    .map(|task| {
                        task.history
                            .into_iter()
                            .filter_map(|event| match event {
                                TaskEvent::Verification { result } => Some(result),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                emitter.emit(TaskEvent::Result {
                    result: TaskResult {
                        source_revision: workspace.revision.clone(),
                        verification,
                        diff,
                    },
                });
            }
            Err(error) => {
                may_cleanup = false;
                emitter.warn(format!(
                    "result capture failed: {error}; task workspace retained for recovery"
                ));
            }
        }
    }
    if may_cleanup
        && let Some(workspace) = workspace
        && let Err(error) = state.workspaces.cleanup(workspace)
    {
        emitter.warn(format!("workspace cleanup failed: {error}"));
        may_cleanup = false;
    }
    if !may_cleanup && let Some(workspace) = workspace {
        schedule_recovery_cleanup(state, workspace, emitter);
    }
    let (status, error) = match result {
        Err(error) => (TaskStatus::Failed, Some(format!("{error:#}"))),
        Ok(()) => {
            let rejected = state
                .manager
                .get(id)
                .is_some_and(|task| task.status == TaskStatus::Rejected);
            (
                if rejected {
                    TaskStatus::Rejected
                } else {
                    TaskStatus::Completed
                },
                None,
            )
        }
    };
    // Send terminal UI updates only after result/cleanup diagnostics are known.
    emitter.emit(TaskEvent::Finished { status, error });
}

fn schedule_recovery_cleanup(state: &AppState, workspace: &TaskWorkspace, emitter: &Emitter) {
    let retention = state.config.execution.recovery_retention;
    emitter.warn(format!("workspace retained for recovery; cleanup retry in {} seconds. Recover needed files before that deadline", retention.as_secs()));
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        emitter.warn("no running cleanup scheduler; manual workspace cleanup required");
        return;
    };
    let provider = state.workspaces.clone();
    let workspace = workspace.clone();
    let emitter = emitter.clone();
    handle.spawn(async move {
        tokio::time::sleep(retention).await;
        let result = tokio::task::spawn_blocking(move || provider.cleanup(&workspace)).await;
        match result {
            Ok(Ok(())) => emitter.notice("recovery retention expired; temporary workspace cleaned"),
            error => emitter.warn(format!(
                "recovery cleanup failed; manual cleanup required: {error:?}"
            )),
        }
    });
}

async fn run(
    state: &AppState,
    id: TaskId,
    emitter: &Emitter,
    workspace: &mut Option<TaskWorkspace>,
) -> Result<()> {
    let task = match state.manager.get(id) {
        Some(task) => task,
        None => return Ok(()),
    };

    // Task 0005: the run uses the selection frozen on the task, and it is
    // resolved BEFORE any workspace or API work so an unavailable provider
    // fails immediately instead of half-way through a build.
    let agents = crate::agent::resolve(&task.agents, &state.config)?;
    emitter.emit(TaskEvent::AgentsSelected {
        agents: task.agents.clone(),
    });

    let project = match task.project_id {
        Some(project_id) => Some(
            state
                .projects
                .get(project_id)
                .ok_or_else(|| anyhow::anyhow!("registered project no longer exists"))?,
        ),
        None => None,
    };

    let (profile, repository_context) = match task.kind {
        TaskKind::NewProject => {
            let technology = task
                .technology
                .clone()
                .ok_or_else(|| anyhow::anyhow!("new project has no selected technology"))?;
            (
                ProjectProfile::selected(technology),
                "New empty project".into(),
            )
        }
        TaskKind::Feature | TaskKind::BugFix => {
            let project = project
                .as_ref()
                .expect("validated existing task has project");
            *workspace = Some(prepare_existing(state, id, project).await?);
            let prepared = workspace.as_ref().expect("workspace was prepared");
            let path = prepared.path.clone();
            let title = task.title.clone();
            let description = task.description.clone();
            let kind = task.kind;
            let inspection = tokio::task::spawn_blocking(move || {
                inspect(
                    &path,
                    InspectionRequest {
                        kind,
                        title: &title,
                        description: &description,
                    },
                )
            })
            .await??;
            let profile = inspection.profile.clone();
            state.projects.set_profile(project.id, profile.clone());
            let context = inspection.prompt_context();
            emitter.emit(TaskEvent::Inspection {
                profile: profile.clone(),
                source_revision: prepared.revision.clone(),
            });
            (profile, context)
        }
    };

    if task.kind == TaskKind::NewProject {
        emitter.emit(TaskEvent::Inspection {
            profile: profile.clone(),
            source_revision: None,
        });
    }

    let topic = format!(
        "{}\n\n{}",
        task.topic(),
        crate::workflow::design_context(task.kind, &profile, &repository_context)
    );
    emitter.status(TaskStatus::Debating);
    let outcome = crate::debate::run(
        agents.proposer.as_ref(),
        agents.critic.as_ref(),
        &topic,
        state.config.max_rounds,
        emitter,
    )
    .await?;

    emitter.status(TaskStatus::GeneratingSpec);
    let document = spec::build(
        agents.proposer.as_ref(),
        agents.critic.as_ref(),
        &outcome.transcript,
        outcome.approved,
        emitter,
    )
    .await?;
    emitter.emit(TaskEvent::Spec {
        markdown: document,
        path: format!("artifacts/{}", spec::APPROVED_SPEC_FILENAME),
    });

    emitter.status(TaskStatus::WaitingForApproval);
    let Some(decision) = state.manager.await_decision(id).await else {
        return Ok(());
    };
    if !decision.approve {
        emitter.notice("rejected; no repository changes were published");
        emitter.status(TaskStatus::Rejected);
        return Ok(());
    }

    if workspace.is_none() {
        let provider = state.workspaces.clone();
        *workspace = Some(
            tokio::task::spawn_blocking(move || {
                provider.prepare(WorkspaceRequest {
                    task_id: id,
                    source: None,
                    revision: None,
                })
            })
            .await??,
        );
    }
    let workspace_ref = workspace.as_ref().expect("workspace was prepared");
    let spec_path = write_approved_spec(&state.manager, id, workspace_ref)?;

    emitter.status(TaskStatus::Implementing);
    let prompt = crate::workflow::implementation_prompt(task.kind, &profile);
    agents
        .worker
        .execute(
            CodingTaskRequest {
                workspace: &workspace_ref.path,
                spec_path: &spec_path,
                instructions: &prompt,
            },
            emitter,
        )
        .await?;

    let commands = crate::verification::plan(&profile, &workspace_ref.path);
    if commands.is_empty() {
        emitter.warn("no automatic verification commands were detected");
    }
    let verification = crate::verification::run_with_limits(
        &commands,
        &workspace_ref.path,
        &state.config.execution,
    )
    .await?;
    for result in &verification {
        emitter.emit(TaskEvent::Verification {
            result: result.clone(),
        });
    }

    let failed = verification.iter().any(|result| !result.success);
    let diff_path = workspace_ref.path.clone();
    let limits = state.config.execution.clone();
    let diff =
        tokio::task::spawn_blocking(move || diff_result_with_limits(&diff_path, &limits)).await??;
    let result = TaskResult {
        source_revision: workspace_ref.revision.clone(),
        verification,
        diff,
    };
    emitter.emit(TaskEvent::Result { result });
    if failed {
        bail!("one or more verification commands failed");
    }

    Ok(())
}

async fn prepare_existing(
    state: &AppState,
    id: TaskId,
    project: &Project,
) -> Result<TaskWorkspace> {
    let provider = state.workspaces.clone();
    let project = project.clone();
    tokio::task::spawn_blocking(move || {
        provider.prepare(WorkspaceRequest {
            task_id: id,
            source: Some(&project.source),
            revision: Some(&project.default_branch),
        })
    })
    .await?
}

fn write_approved_spec(
    manager: &TaskManager,
    id: TaskId,
    workspace: &TaskWorkspace,
) -> Result<std::path::PathBuf> {
    let approved_spec = manager
        .approved_spec(id)
        .ok_or_else(|| anyhow::anyhow!("approved specification is missing from task state"))?;
    spec::write_artifact(&workspace.artifacts(), &approved_spec)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn cleanup_failure_preserves_result_and_retries() {
        use crate::workspace::{LocalWorkspaceProvider, WorkspaceProvider};
        struct RetryCleanup {
            provider: LocalWorkspaceProvider,
            failed_once: std::sync::atomic::AtomicBool,
        }
        impl WorkspaceProvider for RetryCleanup {
            fn prepare(&self, request: WorkspaceRequest<'_>) -> Result<TaskWorkspace> {
                self.provider.prepare(request)
            }
            fn cleanup(&self, workspace: &TaskWorkspace) -> Result<()> {
                if !self
                    .failed_once
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    bail!("simulated cleanup failure");
                }
                self.provider.cleanup(workspace)
            }
        }
        let (mut state, root) = crate::web::tests::test_state("cleanup-retry");
        std::sync::Arc::make_mut(&mut state.config)
            .execution
            .recovery_retention = std::time::Duration::from_millis(50);
        state.workspaces = std::sync::Arc::new(RetryCleanup {
            provider: LocalWorkspaceProvider::new(root.join("task-workspaces")).unwrap(),
            failed_once: std::sync::atomic::AtomicBool::new(false),
        });
        let task = state.manager.create("task", "description", "legacy");
        let workspace = state
            .workspaces
            .prepare(WorkspaceRequest {
                task_id: task.id,
                source: None,
                revision: None,
            })
            .unwrap();
        std::fs::write(workspace.path.join("partial.txt"), "recoverable content\n").unwrap();
        finish_run(
            &state,
            task.id,
            &state.manager.emitter(task.id),
            Some(&workspace),
            Err(anyhow::anyhow!("implementer failed")),
        );
        let task = state.manager.get(task.id).unwrap();
        assert!(task.result.unwrap().diff.contains("+recoverable content"));
        assert_eq!(task.error.as_deref(), Some("implementer failed"));
        assert!(task.history.iter().any(|event| matches!(event, TaskEvent::Warning {message} if message.contains("simulated cleanup failure"))));
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while workspace.root.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    use super::*;
    use crate::task::{Decision, TaskEvent};
    use uuid::Uuid;

    #[test]
    fn partial_implementation_failure_captures_changes_before_cleanup() {
        let (state, root) = crate::web::tests::test_state("partial-result");
        let task = state.manager.create("task", "description", "legacy");
        let emitter = state.manager.emitter(task.id);
        emitter.status(TaskStatus::Implementing);
        let workspace = state
            .workspaces
            .prepare(WorkspaceRequest {
                task_id: task.id,
                source: None,
                revision: None,
            })
            .unwrap();
        std::fs::write(workspace.path.join("partial.py"), "print('partial work')\n").unwrap();
        finish_run(
            &state,
            task.id,
            &emitter,
            Some(&workspace),
            Err(anyhow::anyhow!("implementer failed")),
        );
        let stored = state.manager.get(task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Failed);
        assert!(
            stored
                .result
                .unwrap()
                .diff
                .contains("+print('partial work')")
        );
        assert!(!workspace.path.exists());
        let result_index = stored
            .history
            .iter()
            .position(|event| matches!(event, TaskEvent::Result { .. }))
            .unwrap();
        let finished_index = stored
            .history
            .iter()
            .position(|event| matches!(event, TaskEvent::Finished { .. }))
            .unwrap();
        assert!(result_index < finished_index);
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn missing_verifier_keeps_generated_files_and_verification_failure() {
        let (state, root) = crate::web::tests::test_state("missing-verifier-result");
        let task = state.manager.create("task", "description", "legacy");
        let emitter = state.manager.emitter(task.id);
        let workspace = state
            .workspaces
            .prepare(WorkspaceRequest {
                task_id: task.id,
                source: None,
                revision: None,
            })
            .unwrap();
        std::fs::write(workspace.path.join("main.py"), "print('complete')\n").unwrap();
        let commands = [crate::verification::VerificationCommand {
            program: root
                .join("missing-verifier-executable")
                .to_string_lossy()
                .into_owned(),
            args: vec![],
        }];
        let results = crate::verification::run(&commands, &workspace.path)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        emitter.emit(TaskEvent::Verification {
            result: results[0].clone(),
        });
        finish_run(
            &state,
            task.id,
            &emitter,
            Some(&workspace),
            Err(anyhow::anyhow!("verification failed")),
        );
        let stored = state.manager.get(task.id).unwrap();
        let result = stored.result.unwrap();
        assert!(result.diff.contains("+print('complete')"));
        assert!(result.verification[0].output.contains("could not launch"));
        assert_eq!(stored.status, TaskStatus::Failed);
        assert!(!workspace.path.exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn failed_diff_capture_retains_workspace_for_recovery() {
        let (state, root) = crate::web::tests::test_state("uncaptured-result");
        let task = state.manager.create("task", "description", "legacy");
        let workspace = TaskWorkspace {
            root: root.join("task-workspaces").join(task.id.to_string()),
            path: root
                .join("task-workspaces")
                .join(task.id.to_string())
                .join("repo"),
            revision: None,
        };
        std::fs::create_dir_all(&workspace.path).unwrap();
        std::fs::write(workspace.path.join("recover.txt"), "preserve me").unwrap();
        finish_run(
            &state,
            task.id,
            &state.manager.emitter(task.id),
            Some(&workspace),
            Err(anyhow::anyhow!("execution failed")),
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("recover.txt")).unwrap(),
            "preserve me"
        );
        let stored = state.manager.get(task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Failed);
        assert!(stored.history.iter().any(|event| matches!(event, TaskEvent::Warning { message } if message.contains("retained for recovery"))));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn implementer_spec_file_matches_authoritative_approved_text() {
        let manager = TaskManager::new();
        let task = manager.create("task", "description", "legacy");
        manager.emitter(task.id).emit(TaskEvent::Spec {
            markdown: "generated".into(),
            path: spec::SPEC_FILENAME.into(),
        });
        manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        manager.decide(
            task.id,
            Decision {
                approve: true,
                spec: Some("edited and approved".into()),
            },
        );
        let root = std::env::temp_dir().join(format!("mac-approved-spec-{}", Uuid::new_v4()));
        let provider = crate::workspace::LocalWorkspaceProvider::new(root.clone()).unwrap();
        use crate::workspace::WorkspaceProvider;
        let workspace = provider
            .prepare(WorkspaceRequest {
                task_id: task.id,
                source: None,
                revision: None,
            })
            .unwrap();
        std::fs::write(
            workspace.path.join("SPEC.md"),
            "project-owned specification",
        )
        .unwrap();
        let spec_path = write_approved_spec(&manager, task.id, &workspace).unwrap();

        assert_eq!(
            std::fs::read_to_string(&spec_path).unwrap(),
            manager.approved_spec(task.id).unwrap()
        );
        assert!(!spec_path.starts_with(workspace.path.canonicalize().unwrap()));
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("SPEC.md")).unwrap(),
            "project-owned specification"
        );
        let prompt = crate::implementer::prompt(&spec_path, "Implement the task.");
        assert!(prompt.contains(&serde_json::to_string(&spec_path.to_string_lossy()).unwrap()));
        let changes = crate::workspace::change_set(&workspace.path).unwrap();
        assert_eq!(changes.files.len(), 1);
        assert_eq!(changes.files[0].path, "SPEC.md");
        assert!(!changes.render().contains("edited and approved"));
        provider.cleanup(&workspace).unwrap();
        assert!(!spec_path.exists());
        std::fs::remove_dir_all(root).ok();
    }
}
#[cfg(test)]
mod failure_limit_tests {
    use super::*;
    #[tokio::test]
    async fn verification_failure_and_timeout_preserve_changes_and_diagnostics() {
        for mode in ["fail", "timeout"] {
            let (state, root) = crate::web::tests::test_state(&format!("verify-{mode}"));
            let task = state.manager.create("task", "description", "legacy");
            let emitter = state.manager.emitter(task.id);
            let workspace = state
                .workspaces
                .prepare(WorkspaceRequest {
                    task_id: task.id,
                    source: None,
                    revision: None,
                })
                .unwrap();
            std::fs::write(workspace.path.join(".multiagent-test-probe"), mode).unwrap();
            let command = crate::verification::VerificationCommand {
                program: std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                args: vec![
                    "--exact".into(),
                    "process_runner::tests::child_probe".into(),
                    "--nocapture".into(),
                ],
            };
            let limits = crate::execution_limits::ExecutionLimits {
                verification_timeout: std::time::Duration::from_millis(400),
                ..Default::default()
            };
            let results =
                crate::verification::run_with_limits(&[command], &workspace.path, &limits)
                    .await
                    .unwrap();
            assert!(!results[0].success);
            if mode == "timeout" {
                assert!(results[0].output.contains("timed out"));
                assert!(results[0].output.contains("partial before timeout"));
            } else {
                assert!(results[0].output.contains("useful failure output"));
            }
            emitter.emit(TaskEvent::Verification {
                result: results[0].clone(),
            });
            finish_run(
                &state,
                task.id,
                &emitter,
                Some(&workspace),
                Err(anyhow::anyhow!("verification failed")),
            );
            let stored = state.manager.get(task.id).unwrap();
            assert_eq!(stored.status, TaskStatus::Failed);
            let result = stored.result.unwrap();
            assert!(result.diff.contains("+work before"));
            assert_eq!(result.verification[0].output, results[0].output);
            assert!(!workspace.root.exists());
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn recovery_workspace_is_cleaned_after_the_configured_retention() {
        let (mut state, root) = crate::web::tests::test_state("recovery-retention");
        std::sync::Arc::make_mut(&mut state.config)
            .execution
            .recovery_retention = std::time::Duration::from_millis(25);
        let task = state.manager.create("task", "description", "legacy");
        let workspace = TaskWorkspace {
            root: root.join("task-workspaces").join(task.id.to_string()),
            path: root
                .join("task-workspaces")
                .join(task.id.to_string())
                .join("repo"),
            revision: None,
        };
        std::fs::create_dir_all(&workspace.path).unwrap();
        std::fs::write(workspace.path.join("debug.txt"), "useful evidence").unwrap();
        finish_run(
            &state,
            task.id,
            &state.manager.emitter(task.id),
            Some(&workspace),
            Err(anyhow::anyhow!("execution failed")),
        );
        assert!(workspace.path.join("debug.txt").exists());
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while workspace.root.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            state.manager.get(task.id).unwrap().error.as_deref(),
            Some("execution failed")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
