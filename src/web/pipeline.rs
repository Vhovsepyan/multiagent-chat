//! Repository-backed orchestration in a disposable task workspace.

use anyhow::{Result, bail};

use crate::agent::{CodingAgent, CodingAgentConfig, CodingTaskRequest};
use crate::evidence::{EvidencePayload, EvidenceStatus, WorkerRole, WorkerStage};
use crate::inspection::{InspectionRequest, inspect};
use crate::milestone::plan_from_spec;
use crate::persistence::PersistentDestination;
use crate::project::Project;
use crate::spec;
use crate::task::{
    Emitter, Task, TaskEvent, TaskId, TaskKind, TaskManager, TaskResult, TaskStatus,
};
use crate::technology::ProjectProfile;
use crate::web::AppState;
use crate::workspace::{TaskWorkspace, WorkspaceRequest, task_result_diff};

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
            let error = format!(
                "task finalization failed: {error}; manual workspace recovery may be required"
            );
            report.emit(TaskEvent::Finished {
                status: TaskStatus::Failed,
                error: Some(error.clone()),
            });
            report.emit(TaskEvent::TaskFailed { error });
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
        match task_result_diff(
            &workspace.path,
            workspace.revision.as_deref(),
            &state.config.execution,
        ) {
            Ok(diff) => {
                let verification = state
                    .manager
                    .get(id)
                    .map(|task| {
                        task.history
                            .into_iter()
                            .filter_map(|recorded| match recorded.event {
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
    let cancelled = state.manager.is_cancelled(id);
    let (status, error) = if cancelled {
        (TaskStatus::Cancelled, None)
    } else {
        match result {
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
        }
    };
    // Send terminal UI updates only after result/cleanup diagnostics are known.
    emitter.emit(TaskEvent::Finished {
        status,
        error: error.clone(),
    });
    match status {
        TaskStatus::Completed => emitter.emit(TaskEvent::TaskCompleted),
        TaskStatus::Failed => emitter.emit(TaskEvent::TaskFailed {
            error: error.unwrap_or_else(|| "task failed".into()),
        }),
        TaskStatus::Rejected => {}
        TaskStatus::Cancelled => {}
        _ => {}
    }
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
    emitter.emit(TaskEvent::TaskStarted);

    // Task 0005: the run uses the selection frozen on the task, and it is
    // resolved BEFORE any workspace or API work so an unavailable provider
    // fails immediately instead of half-way through a build.
    let agents = crate::agent::resolve(&task.agents, &state.config)?;
    emitter.emit(TaskEvent::AgentsSelected {
        agents: task.agents.clone(),
    });

    // Task 0010: an unusable persistent destination is reported here, before any
    // agent or workspace work, rather than after a whole run has been paid for.
    let destination = persistent_destination(state, &task)?;
    if let Some(destination) = &destination {
        emitter.notice(format!(
            "this run will keep the finished project at {}",
            destination.display()
        ));
    }

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
    emitter.emit(TaskEvent::SpecGenerated);

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
    let commands = crate::verification::plan(&profile, &workspace_ref.path);
    if commands.is_empty() {
        emitter.warn("no automatic verification commands were detected");
    }
    let approved_spec = state
        .manager
        .approved_spec(id)
        .ok_or_else(|| anyhow::anyhow!("approved specification is missing from task state"))?;
    let milestones = plan_from_spec(&approved_spec, &commands).map_err(anyhow::Error::msg)?;
    // Task 0009: when the run commits, the workspace repository must be in a
    // state where an isolated commit is obviously safe. Checking once, before
    // any worker starts, means a problem is reported instead of repaired.
    if task.git_mode.commits_enabled() {
        let repo = workspace_ref.path.clone();
        let limits = state.config.execution.clone();
        tokio::task::spawn_blocking(move || crate::git::ensure_commit_ready(&repo, &limits))
            .await??;
        emitter.notice("milestone commits enabled for this run");
    }
    emitter.emit(TaskEvent::MilestonePlanCreated {
        milestones: milestones.clone(),
    });
    let total = milestones.len();
    let mut all_verification = Vec::new();
    for milestone in milestones {
        if state.manager.is_cancelled(id) {
            emitter.emit(TaskEvent::MilestoneCancelled {
                id: milestone.id,
                order: milestone.order,
                title: milestone.title,
                reason: "task cancelled before milestone start".into(),
            });
            return Ok(());
        }
        emitter.emit(TaskEvent::MilestoneStarted {
            id: milestone.id.clone(),
            order: milestone.order,
            title: milestone.title.clone(),
            worker_tool: task.agents.worker.tool,
            worker_model: task.agents.worker.model.clone(),
        });
        // One authoritative instruction per milestone (task 0008): scope lives
        // here, not in the common worker prompt.
        let instructions = crate::workflow::milestone_prompt(
            task.kind,
            &profile,
            &milestone,
            total,
            &repository_context,
        );
        if let Err(error) = execute_worker_for_milestone(
            agents.worker.as_ref(),
            &task.agents.worker,
            CodingTaskRequest {
                workspace: &workspace_ref.path,
                spec_path: &spec_path,
                instructions: &instructions,
            },
            emitter,
            Some((&milestone.id, &milestone.title)),
        )
        .await
        {
            let message = format!("{error:#}");
            emitter.emit(TaskEvent::MilestoneFailed {
                id: milestone.id,
                order: milestone.order,
                title: milestone.title,
                verification: Vec::new(),
                worker_result_summary: None,
                error: message,
            });
            return Err(error);
        }
        if state.manager.is_cancelled(id) {
            emitter.emit(TaskEvent::MilestoneCancelled {
                id: milestone.id,
                order: milestone.order,
                title: milestone.title,
                reason: "task cancelled after worker execution".into(),
            });
            return Ok(());
        }
        let verification = match execute_verification(
            &commands,
            &workspace_ref.path,
            &state.config.execution,
            emitter,
        )
        .await
        {
            Ok(verification) => verification,
            Err(error) => {
                emitter.emit(TaskEvent::MilestoneFailed {
                    id: milestone.id,
                    order: milestone.order,
                    title: milestone.title,
                    verification: Vec::new(),
                    worker_result_summary: Some(
                        "Worker completed; verification could not finish.".into(),
                    ),
                    error: format!("{error:#}"),
                });
                return Err(error);
            }
        };
        if state.manager.is_cancelled(id) {
            emitter.emit(TaskEvent::MilestoneCancelled {
                id: milestone.id,
                order: milestone.order,
                title: milestone.title,
                reason: "task cancelled during verification".into(),
            });
            return Ok(());
        }
        all_verification.extend(verification.clone());
        let failed = verification.iter().any(|result| !result.success);
        if failed {
            emitter.emit(TaskEvent::MilestoneFailed {
                id: milestone.id,
                order: milestone.order,
                title: milestone.title,
                verification,
                worker_result_summary: Some("Worker completed; verification failed.".into()),
                error: "one or more verification commands failed".into(),
            });
            let diff_path = workspace_ref.path.clone();
            let limits = state.config.execution.clone();
            let baseline = workspace_ref.revision.clone();
            let diff = tokio::task::spawn_blocking(move || {
                task_result_diff(&diff_path, baseline.as_deref(), &limits)
            })
            .await??;
            emitter.emit(TaskEvent::Result {
                result: TaskResult {
                    source_revision: workspace_ref.revision.clone(),
                    verification: all_verification,
                    diff,
                },
            });
            bail!("one or more verification commands failed");
        }
        // A milestone is finalized only once its commit (when requested)
        // exists: a failure here fails the milestone rather than passing it.
        let commit = {
            let repo = workspace_ref.path.clone();
            let limits = state.config.execution.clone();
            let mode = task.git_mode;
            let planned = milestone.clone();
            let results = verification.clone();
            match tokio::task::spawn_blocking(move || {
                crate::git::commit_milestone_if_enabled(mode, &repo, &planned, &results, &limits)
            })
            .await?
            {
                Ok(commit) => commit,
                Err(error) => {
                    let message = format!("milestone commit failed: {error:#}");
                    emitter.emit(TaskEvent::MilestoneFailed {
                        id: milestone.id,
                        order: milestone.order,
                        title: milestone.title,
                        verification,
                        worker_result_summary: Some(
                            "Worker completed and verification passed; the commit failed.".into(),
                        ),
                        error: message.clone(),
                    });
                    bail!("{message}");
                }
            }
        };
        match commit {
            Some(commit) => emitter.emit(TaskEvent::MilestoneCommitCreated {
                id: milestone.id.clone(),
                order: milestone.order,
                title: milestone.title.clone(),
                commit,
            }),
            None if task.git_mode.commits_enabled() => {
                emitter.notice(format!(
                    "milestone {} changed nothing; no commit was created",
                    milestone.order
                ));
            }
            None => {}
        }
        emitter.emit(TaskEvent::MilestoneCompleted {
            id: milestone.id,
            order: milestone.order,
            title: milestone.title,
            verification,
            worker_result_summary: "Worker completed successfully.".into(),
        });
    }
    let diff_path = workspace_ref.path.clone();
    let limits = state.config.execution.clone();
    let baseline = workspace_ref.revision.clone();
    let diff = tokio::task::spawn_blocking(move || {
        task_result_diff(&diff_path, baseline.as_deref(), &limits)
    })
    .await??;
    let result = TaskResult {
        source_revision: workspace_ref.revision.clone(),
        verification: all_verification,
        diff,
    };
    // The result is recorded BEFORE persistence, so a persistence failure still
    // leaves the full evidence of what the run produced.
    emitter.emit(TaskEvent::Result { result });

    // Task 0010: implementation and verification are done; the project may now
    // leave the disposable workspace for the destination the user chose.
    if let Some(destination) = &destination {
        if state.manager.is_cancelled(id) {
            emitter.notice("task cancelled before the project was persisted");
            return Ok(());
        }
        persist_project(state, emitter, destination, workspace_ref).await?;
    }
    Ok(())
}

/// The destination a persistent New Project will use, or `None` for the
/// temporary review result every other task produces.
fn persistent_destination(state: &AppState, task: &Task) -> Result<Option<PersistentDestination>> {
    if !task.output.is_some_and(|output| output.is_persistent()) {
        return Ok(None);
    }
    let name = task
        .destination
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("a persistent project has no destination folder name"))?;
    let destination =
        PersistentDestination::resolve(state.config.persistent_output_root.as_deref(), name)?;
    destination.ensure_available()?;
    Ok(Some(destination))
}

/// Copy the finished project to its destination and audit the outcome.
///
/// A failure here is a task failure: the destination must never be presented as
/// a completed persistent result when the project did not actually reach it.
async fn persist_project(
    state: &AppState,
    emitter: &Emitter,
    destination: &PersistentDestination,
    workspace: &TaskWorkspace,
) -> Result<()> {
    emitter.emit(TaskEvent::ProjectPersistenceStarted {
        destination: destination.display(),
    });
    let source = workspace.path.clone();
    let target = destination.clone();
    let limits = state.config.execution.clone();
    let persisted =
        match tokio::task::spawn_blocking(move || target.persist(&source, &limits)).await {
            Ok(persisted) => persisted,
            Err(error) => Err(anyhow::anyhow!("project persistence task failed: {error}")),
        };
    match persisted {
        Ok(project) => {
            emitter.emit(TaskEvent::ProjectPersisted {
                destination: project.destination,
                git: project.git,
            });
            Ok(())
        }
        Err(error) => {
            let message = format!("{error:#}");
            emitter.emit(TaskEvent::ProjectPersistenceFailed {
                destination: destination.display(),
                error: message.clone(),
            });
            bail!("could not keep the generated project: {message}")
        }
    }
}

async fn execute_verification(
    commands: &[crate::verification::VerificationCommand],
    root: &std::path::Path,
    limits: &crate::execution_limits::ExecutionLimits,
    emitter: &Emitter,
) -> Result<Vec<crate::verification::VerificationResult>> {
    emitter.emit(TaskEvent::VerificationStarted {
        commands: commands.len(),
    });
    let verification = match crate::verification::run_with_limits(commands, root, limits).await {
        Ok(verification) => verification,
        Err(error) => {
            emitter.emit(TaskEvent::VerificationFailed {
                command: None,
                error: format!("{error:#}"),
            });
            return Err(error);
        }
    };
    for result in &verification {
        emitter.emit(TaskEvent::Verification {
            result: result.clone(),
        });
    }
    if let Some(failed) = verification.iter().find(|result| !result.success) {
        emitter.emit(TaskEvent::VerificationFailed {
            command: Some(failed.command.clone()),
            error: "one or more verification commands failed".into(),
        });
    } else {
        emitter.emit(TaskEvent::VerificationCompleted {
            commands: verification.len(),
        });
    }
    Ok(verification)
}

async fn execute_worker(
    worker: &dyn CodingAgent,
    selection: &CodingAgentConfig,
    request: CodingTaskRequest<'_>,
    emitter: &Emitter,
) -> Result<()> {
    execute_worker_for_milestone(worker, selection, request, emitter, None).await
}

async fn execute_worker_for_milestone(
    worker: &dyn CodingAgent,
    selection: &CodingAgentConfig,
    request: CodingTaskRequest<'_>,
    emitter: &Emitter,
    milestone: Option<(&str, &str)>,
) -> Result<()> {
    let instruction = crate::evidence::worker_instruction(request.instructions);
    emitter.emit(TaskEvent::WorkerStarted {
        tool: selection.tool,
        model: selection.model.clone(),
    });
    let started = std::time::Instant::now();
    if let Err(error) = worker.execute(request, emitter).await {
        let message = format!("{error:#}");
        emitter.record_evidence(EvidencePayload::WorkerExecution {
            role: WorkerRole::Worker,
            stage: WorkerStage::Implementation,
            milestone_id: milestone.map(|(id, _)| id.to_string()),
            milestone_title: milestone.map(|(_, title)| title.to_string()),
            tool: selection.tool,
            model: selection.model.clone(),
            instruction,
            summary: message.clone(),
            status: EvidenceStatus::Failed,
            duration_ms: crate::evidence::elapsed_ms(started),
            truncated: false,
        });
        emitter.emit(TaskEvent::WorkerFailed {
            tool: selection.tool,
            model: selection.model.clone(),
            error: message,
        });
        return Err(error);
    }
    emitter.record_evidence(EvidencePayload::WorkerExecution {
        role: WorkerRole::Worker,
        stage: WorkerStage::Implementation,
        milestone_id: milestone.map(|(id, _)| id.to_string()),
        milestone_title: milestone.map(|(_, title)| title.to_string()),
        tool: selection.tool,
        model: selection.model.clone(),
        instruction,
        summary: "Worker completed successfully.".into(),
        status: EvidenceStatus::Completed,
        duration_ms: crate::evidence::elapsed_ms(started),
        truncated: false,
    });
    emitter.emit(TaskEvent::WorkerCompleted {
        tool: selection.tool,
        model: selection.model.clone(),
    });
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
    #[test]
    fn successful_finalization_records_completion_after_finished_state() {
        let (state, root) = crate::web::tests::test_state("audit-completed");
        let task = state.manager.create("task", "description", "legacy");
        finish_run(
            &state,
            task.id,
            &state.manager.emitter(task.id),
            None,
            Ok(()),
        );

        let stored = state.manager.get(task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Completed);
        let finished = stored
            .history
            .iter()
            .position(|recorded| matches!(recorded.event, TaskEvent::Finished { .. }))
            .unwrap();
        let completed = stored
            .history
            .iter()
            .position(|recorded| matches!(recorded.event, TaskEvent::TaskCompleted))
            .unwrap();
        assert!(finished < completed);
        std::fs::remove_dir_all(root).ok();
    }

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
        assert!(task.log_tail.iter().any(|event| matches!(&event.event, TaskEvent::Warning {message} if message.contains("simulated cleanup failure"))));
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

    struct FailingWorker;

    #[async_trait::async_trait]
    impl CodingAgent for FailingWorker {
        fn tool(&self) -> crate::agent::CodingTool {
            crate::agent::CodingTool::ClaudeCode
        }

        fn model(&self) -> &str {
            "worker-audit-model"
        }

        async fn execute(
            &self,
            _request: CodingTaskRequest<'_>,
            _emitter: &Emitter,
        ) -> Result<crate::agent::CodingTaskResult> {
            bail!("worker execution failed")
        }
    }

    #[tokio::test]
    async fn worker_failure_records_started_then_failed_with_no_completion() {
        let manager = TaskManager::new();
        let task = manager.create("task", "description", "legacy");
        let selection =
            CodingAgentConfig::new(crate::agent::CodingTool::ClaudeCode, "worker-audit-model");
        let root = std::env::temp_dir();

        let result = execute_worker(
            &FailingWorker,
            &selection,
            CodingTaskRequest {
                workspace: &root,
                spec_path: &root.join("approved-spec.md"),
                instructions: "implement",
            },
            &manager.emitter(task.id),
        )
        .await;

        assert!(result.is_err());
        let stored = manager.get(task.id).unwrap();
        let started = stored
            .history
            .iter()
            .position(|recorded| matches!(recorded.event, TaskEvent::WorkerStarted { .. }))
            .unwrap();
        let failed = stored
            .history
            .iter()
            .position(|recorded| matches!(recorded.event, TaskEvent::WorkerFailed { .. }))
            .unwrap();
        assert!(started < failed);
        assert!(
            !stored
                .history
                .iter()
                .any(|recorded| matches!(recorded.event, TaskEvent::WorkerCompleted { .. }))
        );
        assert!(matches!(
            &stored.history[started].event,
            TaskEvent::WorkerStarted { tool, model }
                if *tool == selection.tool && model == &selection.model
        ));
        assert!(matches!(
            &stored.evidence[0].payload,
            crate::evidence::EvidencePayload::WorkerExecution {
                instruction,
                status: crate::evidence::EvidenceStatus::Failed,
                summary,
                ..
            } if instruction.contains("implement") && summary.contains("worker execution failed")
        ));
    }

    #[tokio::test]
    async fn verification_failure_records_started_then_failed_without_completion() {
        let manager = TaskManager::new();
        let task = manager.create("task", "description", "legacy");
        let root = std::env::temp_dir();
        let commands = [crate::verification::VerificationCommand {
            program: root
                .join(format!("missing-audit-verifier-{}", Uuid::new_v4()))
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
        }];

        let results = execute_verification(
            &commands,
            &root,
            &crate::execution_limits::ExecutionLimits::default(),
            &manager.emitter(task.id),
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 1);
        assert!(!results[0].success);
        let stored = manager.get(task.id).unwrap();
        let started = stored
            .history
            .iter()
            .position(|recorded| matches!(recorded.event, TaskEvent::VerificationStarted { .. }))
            .unwrap();
        let failed = stored
            .history
            .iter()
            .position(|recorded| matches!(recorded.event, TaskEvent::VerificationFailed { .. }))
            .unwrap();
        assert!(started < failed);
        assert!(
            !stored
                .history
                .iter()
                .any(|recorded| matches!(recorded.event, TaskEvent::VerificationCompleted { .. }))
        );
    }

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
                .history
                .iter()
                .any(|recorded| matches!(recorded.event, TaskEvent::TaskFailed { .. }))
        );
        assert!(
            !stored
                .history
                .iter()
                .any(|recorded| matches!(recorded.event, TaskEvent::TaskCompleted))
        );
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
            .position(|event| matches!(event.event, TaskEvent::Result { .. }))
            .unwrap();
        let finished_index = stored
            .history
            .iter()
            .position(|event| matches!(event.event, TaskEvent::Finished { .. }))
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
        assert!(stored.log_tail.iter().any(|event| matches!(&event.event, TaskEvent::Warning { message } if message.contains("retained for recovery"))));
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

/// Task 0010: persistent New Project output, from choosing a destination to the
/// evidence the finished run leaves behind.
#[cfg(test)]
mod persistent_output_tests {
    use super::*;
    use crate::agent::AgentSelection;
    use crate::task::{OutputTarget, PersistenceStatus, TaskKind, TaskRequest};
    use crate::technology::TechStack;

    fn new_project(state: &AppState, output: OutputTarget, destination: Option<&str>) -> Task {
        state
            .manager
            .create_from_request(
                TaskRequest {
                    kind: TaskKind::NewProject,
                    title: "Invoice tool".into(),
                    description: "Generate invoices from a CSV file".into(),
                    project_id: None,
                    technology: Some(TechStack::Rust),
                    output: Some(output),
                    destination: destination.map(str::to_string),
                    agents: None,
                    git_mode: None,
                },
                AgentSelection::compiled_defaults(),
            )
            .unwrap()
    }

    fn workspace_with_project(state: &AppState, task: &Task) -> TaskWorkspace {
        let workspace = state
            .workspaces
            .prepare(WorkspaceRequest {
                task_id: task.id,
                source: None,
                revision: None,
            })
            .unwrap();
        std::fs::write(workspace.path.join("main.rs"), "fn main() {}\n").unwrap();
        workspace
    }

    /// The `type` of every recorded event, which is what the audit log exposes.
    fn event_kinds(task: &Task) -> Vec<String> {
        task.history
            .iter()
            .map(|recorded| {
                serde_json::to_value(&recorded.event).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    fn final_report(state: &AppState, id: TaskId) -> String {
        let snapshot = state.manager.evidence_snapshot(id).unwrap();
        crate::evidence::export(&snapshot)
            .unwrap()
            .files
            .into_iter()
            .find(|(name, _)| name == crate::evidence::FINAL_REPORT_FILENAME)
            .expect("the final report is part of every export")
            .1
    }

    /// Required test 1 and 3, and the audit half of required test 8: the project
    /// reaches its destination and stays there once the workspace is cleaned.
    #[tokio::test]
    async fn a_persistent_new_project_outlives_the_temporary_workspace() {
        let (state, root) = crate::web::tests::test_state("persist-success");
        let task = new_project(
            &state,
            OutputTarget::PersistentLocalProject,
            Some("kept-project"),
        );
        let emitter = state.manager.emitter(task.id);
        let destination = persistent_destination(&state, &task)
            .unwrap()
            .expect("a persistent task resolves a destination");
        let workspace = workspace_with_project(&state, &task);

        persist_project(&state, &emitter, &destination, &workspace)
            .await
            .unwrap();
        finish_run(&state, task.id, &emitter, Some(&workspace), Ok(()));

        assert!(
            !workspace.root.exists(),
            "the temporary workspace is still disposable"
        );
        assert_eq!(
            std::fs::read_to_string(destination.path().join("main.rs")).unwrap(),
            "fn main() {}\n"
        );
        let stored = state.manager.get(task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Completed);
        let persistence = stored.persistence.clone().expect("persistence metadata");
        assert_eq!(persistence.status, PersistenceStatus::Persisted);
        assert_eq!(persistence.mode, OutputTarget::PersistentLocalProject);
        assert_eq!(persistence.destination, destination.display());
        // The workspace is a repository, so the persisted project describes one.
        let git = persistence.git.expect("a repository was persisted");
        assert!(!git.has_remote, "no remote may be configured");
        let kinds = event_kinds(&stored);
        assert!(kinds.contains(&"project_persistence_started".to_string()));
        assert!(kinds.contains(&"project_persisted".to_string()));
        let report = final_report(&state, task.id);
        assert!(
            report.contains("Output mode: Persistent local project"),
            "{report}"
        );
        assert!(report.contains("Persisted to"), "{report}");
        std::fs::remove_dir_all(root).ok();
    }

    /// Required test 2: a temporary review result behaves exactly as before —
    /// nothing is written outside the workspace, and nothing is audited.
    #[tokio::test]
    async fn a_temporary_new_project_publishes_nothing_outside_the_workspace() {
        let (state, root) = crate::web::tests::test_state("persist-temporary");
        let task = new_project(&state, OutputTarget::ReviewableResult, None);
        let emitter = state.manager.emitter(task.id);
        assert!(
            persistent_destination(&state, &task).unwrap().is_none(),
            "a review result resolves no destination"
        );
        let workspace = workspace_with_project(&state, &task);

        finish_run(&state, task.id, &emitter, Some(&workspace), Ok(()));

        let stored = state.manager.get(task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Completed);
        assert!(stored.persistence.is_none());
        assert!(
            !event_kinds(&stored)
                .iter()
                .any(|kind| kind.starts_with("project_persist")),
            "no persistence event belongs to a temporary run"
        );
        assert!(!workspace.root.exists());
        assert_eq!(
            std::fs::read_dir(state.config.persistent_output_root.as_ref().unwrap())
                .unwrap()
                .count(),
            0,
            "the configured output root must stay untouched"
        );
        let report = final_report(&state, task.id);
        assert!(
            report.contains("Output mode: Temporary review result"),
            "{report}"
        );
        assert!(report.contains("Not requested"), "{report}");
        std::fs::remove_dir_all(root).ok();
    }

    /// Required tests 7 and 8: a destination that became unusable during the run
    /// fails the task, is audited, and keeps whatever is already there.
    #[tokio::test]
    async fn a_persistence_failure_fails_the_task_and_keeps_the_destination() {
        let (state, root) = crate::web::tests::test_state("persist-failure");
        let task = new_project(
            &state,
            OutputTarget::PersistentLocalProject,
            Some("occupied-later"),
        );
        let emitter = state.manager.emitter(task.id);
        let destination = persistent_destination(&state, &task).unwrap().unwrap();
        let workspace = workspace_with_project(&state, &task);
        // Somebody puts a project there while this run is still building.
        std::fs::create_dir_all(destination.path()).unwrap();
        std::fs::write(destination.path().join("existing.txt"), "user work\n").unwrap();

        let error = persist_project(&state, &emitter, &destination, &workspace)
            .await
            .unwrap_err();
        finish_run(&state, task.id, &emitter, Some(&workspace), Err(error));

        assert_eq!(
            std::fs::read_to_string(destination.path().join("existing.txt")).unwrap(),
            "user work\n",
            "existing destination content must survive"
        );
        assert!(!destination.path().join("main.rs").exists());
        let stored = state.manager.get(task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Failed);
        let persistence = stored.persistence.clone().expect("persistence metadata");
        assert_eq!(persistence.status, PersistenceStatus::Failed);
        assert!(
            persistence.error.as_deref().unwrap().contains("not empty"),
            "{persistence:?}"
        );
        let kinds = event_kinds(&stored);
        assert!(kinds.contains(&"project_persistence_failed".to_string()));
        assert!(!kinds.contains(&"project_persisted".to_string()));
        assert!(!kinds.contains(&"task_completed".to_string()));
        let report = final_report(&state, task.id);
        assert!(report.contains("Persistent result: Failed"), "{report}");
        assert!(!report.contains("Persisted to"), "{report}");
        std::fs::remove_dir_all(root).ok();
    }

    /// An unusable destination stops the run before any agent is called, and an
    /// installation that configures no output root cannot persist at all.
    #[tokio::test]
    async fn an_unusable_destination_is_refused_before_the_run_starts() {
        let (mut state, root) = crate::web::tests::test_state("persist-precheck");
        let task = new_project(
            &state,
            OutputTarget::PersistentLocalProject,
            Some("already-there"),
        );
        let output_root = state.config.persistent_output_root.clone().unwrap();
        std::fs::create_dir_all(output_root.join("already-there")).unwrap();
        std::fs::write(output_root.join("already-there/theirs.txt"), "theirs\n").unwrap();

        let error = persistent_destination(&state, &task)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not empty"), "unexpected: {error}");

        std::sync::Arc::make_mut(&mut state.config).persistent_output_root = None;
        let error = persistent_destination(&state, &task)
            .unwrap_err()
            .to_string();
        assert!(error.contains("not configured"), "unexpected: {error}");
        std::fs::remove_dir_all(root).ok();
    }
}
