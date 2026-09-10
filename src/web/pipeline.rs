//! Repository-backed orchestration in a disposable task workspace.

use anyhow::{Result, bail};

use crate::agent::{ChatAgent, ChatAgentConfig, CodingAgent, CodingAgentConfig, CodingTaskRequest};
use crate::evidence::{EvidencePayload, EvidenceStatus, WorkerRole, WorkerStage};
use crate::inspection::{InspectionRequest, inspect};
use crate::milestone::Milestone;
use crate::milestone::plan_from_spec;
use crate::persistence::PersistentDestination;
use crate::project::Project;
use crate::review::{MilestoneReview, Review, ReviewRequest};
use crate::spec;
use crate::task::{
    AgentStage, Emitter, Task, TaskEvent, TaskId, TaskKind, TaskManager, TaskResult, TaskStatus,
};
use crate::technology::ProjectProfile;
use crate::verification::{VerificationCommand, VerificationResult};
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
        let mut verification = match execute_verification(
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
        if verification.iter().any(|result| !result.success) {
            return MilestoneFailure {
                state,
                emitter,
                workspace: workspace_ref,
                milestone: &milestone,
                verification,
                all_verification,
                summary: "Worker completed; verification failed.",
                error: "one or more verification commands failed".into(),
            }
            .fail()
            .await;
        }

        // Task 0011: the critic now reviews what was actually built, and its
        // findings go back to the worker for a bounded fix cycle. A milestone
        // is only finalized once that review passes.
        let review = ReviewLoop {
            state,
            id,
            emitter,
            critic: agents.critic.as_ref(),
            critic_selection: &task.agents.critic,
            worker: agents.worker.as_ref(),
            worker_selection: &task.agents.worker,
            kind: task.kind,
            profile: &profile,
            approved_spec: &approved_spec,
            spec_path: &spec_path,
            workspace: workspace_ref,
            commands: &commands,
            total,
        }
        .run(&milestone, &mut verification, &mut all_verification)
        .await?;
        let Some(review) = review else {
            // The loop already recorded why the milestone stopped.
            return Ok(());
        };
        let worker_summary = if review.iterations_used == 0 {
            "Worker completed successfully.".to_string()
        } else {
            format!(
                "Worker completed successfully; {} review fix iteration(s) applied and re-verified.",
                review.iterations_used
            )
        };
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
            worker_result_summary: worker_summary,
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
            // The project is at its destination; only its metadata is missing.
            if let Some(warning) = &project.git_warning {
                emitter.warn(warning.clone());
            }
            emitter.emit(TaskEvent::ProjectPersisted {
                destination: project.destination,
                git: project.git,
                git_warning: project.git_warning,
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
    execute_worker_stage(worker, selection, request, emitter, milestone, None).await
}

/// Which correction run this is, when the worker is fixing review findings
/// rather than implementing a milestone (task 0011).
#[derive(Debug, Clone, Copy)]
struct FixRun {
    order: u32,
    iteration: u32,
    of: u32,
}

/// One worker run, whether it implements a milestone or corrects a review.
///
/// The two differ only in which lifecycle events the audit gets and how the
/// evidence record is staged; the execution, bounding and redaction path is
/// deliberately the same one.
async fn execute_worker_stage(
    worker: &dyn CodingAgent,
    selection: &CodingAgentConfig,
    request: CodingTaskRequest<'_>,
    emitter: &Emitter,
    milestone: Option<(&str, &str)>,
    fix: Option<FixRun>,
) -> Result<()> {
    let instruction = crate::evidence::worker_instruction(request.instructions);
    let milestone_id = || milestone.map(|(id, _)| id.to_string()).unwrap_or_default();
    let stage = match fix {
        Some(_) => WorkerStage::Fix,
        None => WorkerStage::Implementation,
    };
    match fix {
        Some(fix) => emitter.emit(TaskEvent::FixStarted {
            milestone_id: milestone_id(),
            order: fix.order,
            iteration: fix.iteration,
            of: fix.of,
            tool: selection.tool,
            model: selection.model.clone(),
        }),
        None => emitter.emit(TaskEvent::WorkerStarted {
            tool: selection.tool,
            model: selection.model.clone(),
        }),
    }
    let started = std::time::Instant::now();
    if let Err(error) = worker.execute(request, emitter).await {
        let message = format!("{error:#}");
        emitter.record_evidence(EvidencePayload::WorkerExecution {
            role: WorkerRole::Worker,
            stage,
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
        match fix {
            Some(fix) => emitter.emit(TaskEvent::FixFailed {
                milestone_id: milestone_id(),
                order: fix.order,
                iteration: fix.iteration,
                of: fix.of,
                tool: selection.tool,
                model: selection.model.clone(),
                error: message,
            }),
            None => emitter.emit(TaskEvent::WorkerFailed {
                tool: selection.tool,
                model: selection.model.clone(),
                error: message,
            }),
        }
        return Err(error);
    }
    emitter.record_evidence(EvidencePayload::WorkerExecution {
        role: WorkerRole::Worker,
        stage,
        milestone_id: milestone.map(|(id, _)| id.to_string()),
        milestone_title: milestone.map(|(_, title)| title.to_string()),
        tool: selection.tool,
        model: selection.model.clone(),
        instruction,
        summary: match fix {
            Some(fix) => format!("Worker completed review fix iteration {}.", fix.iteration),
            None => "Worker completed successfully.".into(),
        },
        status: EvidenceStatus::Completed,
        duration_ms: crate::evidence::elapsed_ms(started),
        truncated: false,
    });
    match fix {
        Some(fix) => emitter.emit(TaskEvent::FixCompleted {
            milestone_id: milestone_id(),
            order: fix.order,
            iteration: fix.iteration,
            of: fix.of,
            tool: selection.tool,
            model: selection.model.clone(),
        }),
        None => emitter.emit(TaskEvent::WorkerCompleted {
            tool: selection.tool,
            model: selection.model.clone(),
        }),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Task 0011: implementation review and the bounded fix loop
// ---------------------------------------------------------------------------

/// Ending one milestone in failure while still publishing what it produced.
///
/// The task result is recorded before the run stops, so a failed milestone is
/// still reviewable — the behavior verification failures already had.
struct MilestoneFailure<'a> {
    state: &'a AppState,
    emitter: &'a Emitter,
    workspace: &'a TaskWorkspace,
    milestone: &'a Milestone,
    verification: Vec<VerificationResult>,
    all_verification: Vec<VerificationResult>,
    summary: &'a str,
    error: String,
}

impl MilestoneFailure<'_> {
    /// Always returns `Err`: the caller ends the milestone with it.
    async fn fail(self) -> Result<()> {
        self.emitter.emit(TaskEvent::MilestoneFailed {
            id: self.milestone.id.clone(),
            order: self.milestone.order,
            title: self.milestone.title.clone(),
            verification: self.verification,
            worker_result_summary: Some(self.summary.to_string()),
            error: self.error.clone(),
        });
        let diff_path = self.workspace.path.clone();
        let limits = self.state.config.execution.clone();
        let baseline = self.workspace.revision.clone();
        let diff = tokio::task::spawn_blocking(move || {
            task_result_diff(&diff_path, baseline.as_deref(), &limits)
        })
        .await??;
        self.emitter.emit(TaskEvent::Result {
            result: TaskResult {
                source_revision: self.workspace.revision.clone(),
                verification: self.all_verification,
                diff,
            },
        });
        bail!("{}", self.error)
    }
}

/// The critic reviewing an implemented milestone, and the worker correcting
/// what it finds, for at most the configured number of iterations (task 0011).
struct ReviewLoop<'a> {
    state: &'a AppState,
    id: TaskId,
    emitter: &'a Emitter,
    /// The critic and worker frozen on the task (task 0005), so a review and a
    /// fix use exactly the agents the run was created with.
    critic: &'a dyn ChatAgent,
    critic_selection: &'a ChatAgentConfig,
    worker: &'a dyn CodingAgent,
    worker_selection: &'a CodingAgentConfig,
    kind: TaskKind,
    profile: &'a ProjectProfile,
    approved_spec: &'a str,
    spec_path: &'a std::path::Path,
    workspace: &'a TaskWorkspace,
    commands: &'a [VerificationCommand],
    total: usize,
}

impl ReviewLoop<'_> {
    /// Review, fix, re-verify, review again — until the critic passes, the
    /// iterations run out, or something fails.
    ///
    /// `verification` carries this milestone's current results in and the
    /// latest ones out; `all_verification` accumulates every run for the task
    /// result. `Ok(None)` means the task was cancelled and the reason is
    /// already recorded.
    async fn run(
        &self,
        milestone: &Milestone,
        verification: &mut Vec<VerificationResult>,
        all_verification: &mut Vec<VerificationResult>,
    ) -> Result<Option<MilestoneReview>> {
        let of = self.state.config.max_fix_iterations;
        let mut iteration = 0;
        let mut worker_summary = "Worker completed successfully.".to_string();
        loop {
            if self.cancelled(milestone, "task cancelled before implementation review") {
                return Ok(None);
            }
            let diff = match self.change_so_far().await {
                Ok(diff) => diff,
                Err(error) => {
                    return self
                        .fail(
                            milestone,
                            verification,
                            all_verification,
                            "Worker completed and verification passed; the change could not be read for review.",
                            format!("implementation review could not read the change: {error:#}"),
                        )
                        .await;
                }
            };
            let review = match self
                .review(
                    milestone,
                    &diff,
                    verification,
                    &worker_summary,
                    iteration,
                    of,
                )
                .await
            {
                Ok(review) => review,
                Err(error) => {
                    return self
                        .fail(
                            milestone,
                            verification,
                            all_verification,
                            "Worker completed and verification passed; the implementation review failed.",
                            format!("implementation review failed: {error:#}"),
                        )
                        .await;
                }
            };
            if review.status.is_pass() {
                return Ok(Some(MilestoneReview {
                    status: review.status,
                    iterations_used: iteration,
                    max_iterations: of,
                    findings: review.findings,
                }));
            }
            if iteration >= of {
                // Unresolved findings never become a successful milestone.
                return self
                    .fail(
                        milestone,
                        verification,
                        all_verification,
                        "Worker completed, but the implementation review still requires fixes.",
                        format!(
                            "implementation review still requires fixes after {of} fix iteration(s); human review is required"
                        ),
                    )
                    .await;
            }
            iteration += 1;
            if self.cancelled(milestone, "task cancelled before a review fix") {
                return Ok(None);
            }
            if let Err(error) = self.fix(milestone, &review, iteration, of).await {
                return self
                    .fail(
                        milestone,
                        verification,
                        all_verification,
                        "Worker failed while correcting the implementation review findings.",
                        format!("review fix iteration {iteration} failed: {error:#}"),
                    )
                    .await;
            }
            worker_summary = format!("Worker applied review fix iteration {iteration} of {of}.");
            if self.cancelled(milestone, "task cancelled after a review fix") {
                return Ok(None);
            }
            // Every fix is re-verified before it is reviewed again.
            *verification = match execute_verification(
                self.commands,
                &self.workspace.path,
                &self.state.config.execution,
                self.emitter,
            )
            .await
            {
                Ok(results) => results,
                Err(error) => {
                    return self
                        .fail(
                            milestone,
                            &mut Vec::new(),
                            all_verification,
                            "Worker fixed the findings; verification could not finish.",
                            format!(
                                "verification could not finish after review fix iteration {iteration}: {error:#}"
                            ),
                        )
                        .await;
                }
            };
            all_verification.extend(verification.clone());
            if verification.iter().any(|result| !result.success) {
                return self
                    .fail(
                        milestone,
                        verification,
                        all_verification,
                        "Worker fixed the findings; verification then failed.",
                        format!(
                            "one or more verification commands failed after review fix iteration {iteration}"
                        ),
                    )
                    .await;
            }
        }
    }

    /// One critic pass over the implemented milestone.
    async fn review(
        &self,
        milestone: &Milestone,
        diff: &str,
        verification: &[VerificationResult],
        worker_summary: &str,
        iteration: u32,
        of: u32,
    ) -> Result<Review> {
        self.emitter.emit(TaskEvent::ImplementationReviewStarted {
            milestone_id: milestone.id.clone(),
            order: milestone.order,
            iteration,
            of,
            provider: self.critic_selection.provider,
            model: self.critic_selection.model.clone(),
        });
        let message = ReviewRequest {
            kind: self.kind,
            milestone,
            total: self.total,
            approved_spec: self.approved_spec,
            diff,
            verification,
            worker_summary,
            iteration,
            max_iterations: of,
        }
        .message();
        let messages = vec![crate::api::Message::user(message)];
        let prompt = crate::evidence::chat_prompt(Some(crate::review::REVIEW_SYSTEM), &messages);
        let started = std::time::Instant::now();
        let outcome = self
            .critic
            .complete_text(Some(crate::review::REVIEW_SYSTEM), &messages)
            .await
            .map_err(|error| format!("{error:#}"))
            .and_then(|reply| {
                // The reply is kept as evidence even when it is unusable.
                crate::review::parse(&reply)
                    .map(|review| (reply.clone(), review))
                    .map_err(|error| format!("{error}; reply was: {reply}"))
            });
        match outcome {
            Ok((reply, review)) => {
                self.emitter
                    .record_evidence(EvidencePayload::AgentInteraction {
                        stage: AgentStage::ImplementationReview,
                        role: crate::evidence::EvidenceRole::Critic,
                        round: Some(iteration + 1),
                        provider: self.critic_selection.provider,
                        model: self.critic_selection.model.clone(),
                        prompt,
                        response: Some(reply),
                        status: EvidenceStatus::Completed,
                        error: None,
                        duration_ms: crate::evidence::elapsed_ms(started),
                        truncated: false,
                    });
                self.emitter.emit(TaskEvent::ImplementationReviewCompleted {
                    milestone_id: milestone.id.clone(),
                    order: milestone.order,
                    iteration,
                    of,
                    status: review.status,
                    findings: review.findings.clone(),
                });
                Ok(review)
            }
            Err(message) => {
                self.emitter
                    .record_evidence(EvidencePayload::AgentInteraction {
                        stage: AgentStage::ImplementationReview,
                        role: crate::evidence::EvidenceRole::Critic,
                        round: Some(iteration + 1),
                        provider: self.critic_selection.provider,
                        model: self.critic_selection.model.clone(),
                        prompt,
                        response: None,
                        status: EvidenceStatus::Failed,
                        error: Some(message.clone()),
                        duration_ms: crate::evidence::elapsed_ms(started),
                        truncated: false,
                    });
                self.emitter.emit(TaskEvent::ImplementationReviewFailed {
                    milestone_id: milestone.id.clone(),
                    order: milestone.order,
                    iteration,
                    of,
                    provider: self.critic_selection.provider,
                    model: self.critic_selection.model.clone(),
                    error: message.clone(),
                });
                bail!("{message}")
            }
        }
    }

    /// Hand the findings back to the worker, and nothing else.
    async fn fix(
        &self,
        milestone: &Milestone,
        review: &Review,
        iteration: u32,
        of: u32,
    ) -> Result<()> {
        let instructions = crate::workflow::fix_prompt(
            self.kind,
            self.profile,
            milestone,
            self.total,
            &review.findings_text(),
            iteration,
            of,
        );
        execute_worker_stage(
            self.worker,
            self.worker_selection,
            CodingTaskRequest {
                workspace: &self.workspace.path,
                spec_path: self.spec_path,
                instructions: &instructions,
            },
            self.emitter,
            Some((&milestone.id, &milestone.title)),
            Some(FixRun {
                order: milestone.order,
                iteration,
                of,
            }),
        )
        .await
    }

    /// What the run has produced so far, bounded for a prompt.
    async fn change_so_far(&self) -> Result<String> {
        let path = self.workspace.path.clone();
        let limits = self.state.config.execution.clone();
        let baseline = self.workspace.revision.clone();
        let diff = tokio::task::spawn_blocking(move || {
            task_result_diff(&path, baseline.as_deref(), &limits)
        })
        .await??;
        Ok(crate::execution_limits::bounded_text(
            &diff,
            crate::review::REVIEW_DIFF_BYTES,
        ))
    }

    fn cancelled(&self, milestone: &Milestone, reason: &str) -> bool {
        if !self.state.manager.is_cancelled(self.id) {
            return false;
        }
        self.emitter.emit(TaskEvent::MilestoneCancelled {
            id: milestone.id.clone(),
            order: milestone.order,
            title: milestone.title.clone(),
            reason: reason.into(),
        });
        true
    }

    /// End the milestone in failure, preserving the work and the evidence.
    async fn fail(
        &self,
        milestone: &Milestone,
        verification: &mut Vec<VerificationResult>,
        all_verification: &mut [VerificationResult],
        summary: &str,
        error: String,
    ) -> Result<Option<MilestoneReview>> {
        MilestoneFailure {
            state: self.state,
            emitter: self.emitter,
            workspace: self.workspace,
            milestone,
            verification: std::mem::take(verification),
            all_verification: all_verification.to_vec(),
            summary,
            error,
        }
        .fail()
        .await
        .map(|()| None)
    }
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

    /// Regression: repository metadata that cannot be read AFTER the project was
    /// published is a warning about reporting. The run must stay completed, the
    /// audit must say the project was persisted, and nothing may claim the
    /// destination was left unchanged.
    #[tokio::test]
    async fn unreadable_git_metadata_does_not_fail_a_published_project() {
        let (mut state, root) = crate::web::tests::test_state("persist-git-metadata");
        // Every Git command times out, so the project is copied and renamed
        // normally and only describing its repository fails.
        std::sync::Arc::make_mut(&mut state.config)
            .execution
            .git_timeout = std::time::Duration::from_nanos(1);
        let task = new_project(
            &state,
            OutputTarget::PersistentLocalProject,
            Some("published-project"),
        );
        let emitter = state.manager.emitter(task.id);
        let destination = persistent_destination(&state, &task).unwrap().unwrap();
        let workspace = workspace_with_project(&state, &task);

        persist_project(&state, &emitter, &destination, &workspace)
            .await
            .expect("a published project is not a failed persistence");
        finish_run(&state, task.id, &emitter, Some(&workspace), Ok(()));

        assert_eq!(
            std::fs::read_to_string(destination.path().join("main.rs")).unwrap(),
            "fn main() {}\n",
            "the project really is at its destination"
        );
        let stored = state.manager.get(task.id).unwrap();
        assert_eq!(stored.status, TaskStatus::Completed);
        let persistence = stored.persistence.clone().expect("persistence metadata");
        assert_eq!(persistence.status, PersistenceStatus::Persisted);
        assert!(persistence.git.is_none());
        assert!(
            persistence
                .git_warning
                .as_deref()
                .is_some_and(|warning| warning.contains("could not be described")),
            "{persistence:?}"
        );
        assert!(persistence.error.is_none(), "{persistence:?}");
        let kinds = event_kinds(&stored);
        assert!(kinds.contains(&"project_persisted".to_string()));
        assert!(!kinds.contains(&"project_persistence_failed".to_string()));
        assert!(kinds.contains(&"task_completed".to_string()));
        // The warning is visible, without pretending the run failed.
        assert!(
            stored.log_tail.iter().any(|recorded| matches!(
                &recorded.event,
                TaskEvent::Warning { message } if message.contains("could not be described")
            )),
            "the non-fatal warning must reach the run log"
        );
        let report = final_report(&state, task.id);
        assert!(report.contains("Persisted to"), "{report}");
        assert!(
            report.contains("Repository metadata unavailable"),
            "{report}"
        );
        assert!(
            !report.contains("No Git repository was present"),
            "an unreadable repository is not an absent one: {report}"
        );
        assert!(
            !report.contains("The destination was left unchanged"),
            "the destination WAS published: {report}"
        );
        std::fs::remove_dir_all(root).ok();
    }
}

/// Task 0011: the critic reviewing what was actually built, and the bounded
/// fix cycle its findings drive.
#[cfg(test)]
mod review_loop_tests {
    use super::*;
    use crate::agent::chat::ScriptedAgent;
    use crate::agent::{AgentSelection, ChatProvider, CodingTaskResult, CodingTool};
    use crate::milestone::{Milestone, MilestoneStatus};
    use crate::review::{ReviewStatus, Severity};
    use crate::task::{TaskKind, TaskRequest};
    use crate::technology::TechStack;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    const PASS: &str = r#"{"status":"PASS","findings":[]}"#;
    const FIX: &str = r#"{"status":"FIX_REQUIRED","findings":[{"requirement":"Spec step 1",
        "severity":"blocker","evidence":"the module is missing","correction":"add the invoice module"}]}"#;

    /// A `CodingAgent` that replays canned outcomes and records its prompts.
    struct ScriptedWorker {
        model: String,
        outcomes: Mutex<VecDeque<std::result::Result<(), String>>>,
        seen: Mutex<Vec<String>>,
    }

    impl ScriptedWorker {
        fn new(outcomes: Vec<std::result::Result<(), String>>) -> Self {
            Self {
                model: "scripted-worker".into(),
                outcomes: Mutex::new(outcomes.into()),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.seen.lock().unwrap().len()
        }

        fn instruction(&self, index: usize) -> String {
            self.seen.lock().unwrap()[index].clone()
        }
    }

    #[async_trait::async_trait]
    impl CodingAgent for ScriptedWorker {
        fn tool(&self) -> CodingTool {
            CodingTool::ClaudeCode
        }

        fn model(&self) -> &str {
            &self.model
        }

        async fn execute(
            &self,
            request: CodingTaskRequest<'_>,
            _emitter: &Emitter,
        ) -> Result<CodingTaskResult> {
            self.seen
                .lock()
                .unwrap()
                .push(request.instructions.to_string());
            match self.outcomes.lock().unwrap().pop_front() {
                Some(Ok(())) => Ok(CodingTaskResult {
                    tool: CodingTool::ClaudeCode,
                    model: self.model.clone(),
                }),
                Some(Err(error)) => bail!("{error}"),
                None => bail!("the scripted worker ran out of outcomes"),
            }
        }
    }

    /// One task, one prepared workspace, one running milestone.
    struct Harness {
        state: AppState,
        root: std::path::PathBuf,
        task: Task,
        emitter: Emitter,
        workspace: TaskWorkspace,
        spec_path: std::path::PathBuf,
        profile: ProjectProfile,
        milestone: Milestone,
    }

    impl Harness {
        fn new(tag: &str) -> Self {
            let (state, root) = crate::web::tests::test_state(tag);
            let task = state
                .manager
                .create_from_request(
                    TaskRequest {
                        kind: TaskKind::Feature,
                        title: "Invoice module".into(),
                        description: "Add invoices".into(),
                        project_id: Some(uuid::Uuid::new_v4()),
                        technology: None,
                        output: None,
                        destination: None,
                        agents: None,
                        git_mode: None,
                    },
                    AgentSelection {
                        proposer: crate::agent::ChatAgentConfig::new(
                            ChatProvider::Gemini,
                            "configured-proposer-model",
                        ),
                        critic: crate::agent::ChatAgentConfig::new(
                            ChatProvider::Anthropic,
                            "configured-critic-model",
                        ),
                        worker: CodingAgentConfig::new(
                            CodingTool::ClaudeCode,
                            "configured-worker-model",
                        ),
                    },
                )
                .unwrap();
            let emitter = state.manager.emitter(task.id);
            let workspace = state
                .workspaces
                .prepare(WorkspaceRequest {
                    task_id: task.id,
                    source: None,
                    revision: None,
                })
                .unwrap();
            std::fs::write(workspace.path.join("invoice.rs"), "fn invoice() {}\n").unwrap();
            let milestone = Milestone {
                id: "m1".into(),
                order: 1,
                title: "Invoice module".into(),
                objective: "Add the invoice module".into(),
                verification_instructions: vec!["cargo test".into()],
                status: MilestoneStatus::Running,
                started_at: None,
                completed_at: None,
                worker_result_summary: None,
                commit: None,
                review: None,
            };
            // The plan has to exist in task state for the milestone to carry
            // its review disposition.
            emitter.emit(TaskEvent::MilestonePlanCreated {
                milestones: vec![milestone.clone()],
            });
            let spec_path = workspace.artifacts().join("approved-spec.md");
            Harness {
                state,
                root,
                task,
                emitter,
                workspace,
                spec_path,
                profile: ProjectProfile::selected(TechStack::Rust),
                milestone,
            }
        }

        fn review_loop<'a>(
            &'a self,
            critic: &'a dyn ChatAgent,
            worker: &'a dyn CodingAgent,
            commands: &'a [VerificationCommand],
        ) -> ReviewLoop<'a> {
            ReviewLoop {
                state: &self.state,
                id: self.task.id,
                emitter: &self.emitter,
                critic,
                critic_selection: &self.task.agents.critic,
                worker,
                worker_selection: &self.task.agents.worker,
                kind: self.task.kind,
                profile: &self.profile,
                approved_spec: "## Steps\n1. Add the invoice module",
                spec_path: &self.spec_path,
                workspace: &self.workspace,
                commands,
                total: 1,
            }
        }

        fn passing_verification(&self) -> Vec<VerificationResult> {
            vec![VerificationResult {
                command: "cargo test".into(),
                success: true,
                output: "ok".into(),
            }]
        }

        fn stored(&self) -> Task {
            self.state.manager.get(self.task.id).unwrap()
        }

        fn event_kinds(&self) -> Vec<String> {
            self.stored()
                .history
                .iter()
                .map(|recorded| {
                    serde_json::to_value(&recorded.event).unwrap()["type"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    /// Required test 1: a passing review finishes the milestone as before.
    #[tokio::test]
    async fn a_passing_review_needs_no_fix() {
        let harness = Harness::new("review-pass");
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[PASS]);
        let worker = ScriptedWorker::new(vec![]);
        let mut verification = harness.passing_verification();
        let mut all = verification.clone();

        let review = harness
            .review_loop(&critic, &worker, &[])
            .run(&harness.milestone, &mut verification, &mut all)
            .await
            .unwrap()
            .expect("a completed review");

        assert_eq!(review.status, ReviewStatus::Pass);
        assert_eq!(review.iterations_used, 0);
        assert_eq!(review.max_iterations, 2);
        assert_eq!(worker.calls(), 0, "a passing review never runs a fix");
        let kinds = harness.event_kinds();
        assert!(kinds.contains(&"implementation_review_started".to_string()));
        assert!(kinds.contains(&"implementation_review_completed".to_string()));
        assert!(!kinds.iter().any(|kind| kind.starts_with("fix_")));
        // The critic reviewed the real change, not only the design.
        let (system, messages) = critic.call(0);
        let sent = format!("{}{}", system.unwrap_or_default(), messages[0].content);
        assert!(sent.contains("reviewing an IMPLEMENTATION"), "{sent}");
        assert!(sent.contains("fn invoice() {}"), "{sent}");
        assert!(sent.contains("Add the invoice module"), "{sent}");
        assert!(sent.contains("`cargo test` — passed"), "{sent}");
        let stored = harness.stored();
        assert_eq!(
            stored.milestones[0].review.as_ref().unwrap().status,
            ReviewStatus::Pass
        );
    }

    /// Required test 2: findings go back to the worker, and the re-review passes.
    #[tokio::test]
    async fn findings_are_fixed_and_then_pass() {
        let harness = Harness::new("review-fix-pass");
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[FIX, PASS]);
        let worker = ScriptedWorker::new(vec![Ok(())]);
        let mut verification = harness.passing_verification();
        let mut all = verification.clone();

        let review = harness
            .review_loop(&critic, &worker, &[])
            .run(&harness.milestone, &mut verification, &mut all)
            .await
            .unwrap()
            .expect("a completed review");

        assert_eq!(review.status, ReviewStatus::Pass);
        assert_eq!(review.iterations_used, 1);
        assert_eq!(worker.calls(), 1);
        // The worker was asked for exactly the reported correction.
        let instruction = worker.instruction(0);
        assert!(
            instruction.contains("Fix ONLY the findings"),
            "{instruction}"
        );
        assert!(
            instruction.contains("add the invoice module"),
            "{instruction}"
        );
        assert!(
            instruction.contains("fix iteration 1 of 2"),
            "{instruction}"
        );
        let kinds = harness.event_kinds();
        let order = |name: &str| kinds.iter().position(|kind| kind == name).unwrap();
        assert!(order("implementation_review_completed") < order("fix_started"));
        assert!(order("fix_started") < order("fix_completed"));
        // Verification reran after the fix, before the second review.
        assert!(order("fix_completed") < order("verification_started"));
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| *kind == "implementation_review_completed")
                .count(),
            2,
            "the critic re-reviews after a fix"
        );
        let stored = harness.stored();
        let recorded = stored.milestones[0].review.as_ref().unwrap();
        assert_eq!(recorded.status, ReviewStatus::Pass);
        assert_eq!(recorded.iterations_used, 1);
    }

    /// Required test 3: the loop is bounded, and unresolved findings never pass.
    #[tokio::test]
    async fn unresolved_findings_fail_the_milestone_after_the_last_iteration() {
        let harness = Harness::new("review-exhausted");
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[FIX, FIX, FIX]);
        let worker = ScriptedWorker::new(vec![Ok(()), Ok(())]);
        let mut verification = harness.passing_verification();
        let mut all = verification.clone();

        let error = harness
            .review_loop(&critic, &worker, &[])
            .run(&harness.milestone, &mut verification, &mut all)
            .await
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("still requires fixes after 2 fix iteration(s)"),
            "{error}"
        );
        assert!(error.contains("human review is required"), "{error}");
        assert_eq!(
            worker.calls(),
            2,
            "no more fixes than the configured maximum"
        );
        assert_eq!(critic.calls(), 3);
        let kinds = harness.event_kinds();
        assert!(kinds.contains(&"milestone_failed".to_string()));
        assert!(!kinds.contains(&"milestone_completed".to_string()));
        // The work is still published for review.
        assert!(kinds.contains(&"result".to_string()));
        let stored = harness.stored();
        let recorded = stored.milestones[0].review.as_ref().unwrap();
        assert_eq!(recorded.status, ReviewStatus::FixRequired);
        assert_eq!(recorded.findings[0].severity, Severity::Blocker);
    }

    /// Required test 4: a worker that cannot apply the fix fails the milestone.
    #[tokio::test]
    async fn a_failing_fix_fails_the_milestone() {
        let harness = Harness::new("review-fix-failure");
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[FIX]);
        let worker = ScriptedWorker::new(vec![Err("worker crashed".into())]);
        let mut verification = harness.passing_verification();
        let mut all = verification.clone();

        let error = harness
            .review_loop(&critic, &worker, &[])
            .run(&harness.milestone, &mut verification, &mut all)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("review fix iteration 1 failed"), "{error}");
        assert!(error.contains("worker crashed"), "{error}");
        let kinds = harness.event_kinds();
        assert!(kinds.contains(&"fix_started".to_string()));
        assert!(kinds.contains(&"fix_failed".to_string()));
        assert!(!kinds.contains(&"fix_completed".to_string()));
        assert!(kinds.contains(&"milestone_failed".to_string()));
        assert!(!kinds.contains(&"milestone_completed".to_string()));
    }

    /// Required test 5: a critic that fails, or that answers with prose instead
    /// of a structured result, stops the milestone rather than passing it.
    #[tokio::test]
    async fn a_critic_failure_fails_the_milestone() {
        for (tag, replies) in [
            ("review-critic-prose", vec!["Looks fine to me, ship it."]),
            ("review-critic-error", Vec::new()),
        ] {
            let harness = Harness::new(tag);
            let critic = ScriptedAgent::new(ChatProvider::Anthropic, &replies);
            let worker = ScriptedWorker::new(vec![]);
            let mut verification = harness.passing_verification();
            let mut all = verification.clone();

            let error = harness
                .review_loop(&critic, &worker, &[])
                .run(&harness.milestone, &mut verification, &mut all)
                .await
                .unwrap_err()
                .to_string();

            assert!(error.contains("implementation review failed"), "{error}");
            let kinds = harness.event_kinds();
            assert!(kinds.contains(&"implementation_review_failed".to_string()));
            assert!(!kinds.contains(&"implementation_review_completed".to_string()));
            assert!(kinds.contains(&"milestone_failed".to_string()));
            assert!(!kinds.contains(&"milestone_completed".to_string()));
            assert_eq!(worker.calls(), 0);
        }
    }

    /// Required test 6: verification that fails after a fix fails the milestone.
    #[tokio::test]
    async fn verification_failure_after_a_fix_fails_the_milestone() {
        let harness = Harness::new("review-verify-after-fix");
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[FIX, PASS]);
        let worker = ScriptedWorker::new(vec![Ok(())]);
        let commands = [VerificationCommand {
            program: harness
                .root
                .join("missing-verifier-executable")
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
        }];
        let mut verification = harness.passing_verification();
        let mut all = verification.clone();

        let error = harness
            .review_loop(&critic, &worker, &commands)
            .run(&harness.milestone, &mut verification, &mut all)
            .await
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("failed after review fix iteration 1"),
            "{error}"
        );
        assert_eq!(
            critic.calls(),
            1,
            "a failed re-verification is not reviewed"
        );
        let kinds = harness.event_kinds();
        assert!(kinds.contains(&"verification_failed".to_string()));
        assert!(kinds.contains(&"milestone_failed".to_string()));
        assert!(!kinds.contains(&"milestone_completed".to_string()));
        // Every verification run is part of the published task result.
        let stored = harness.stored();
        assert!(stored.result.unwrap().verification.len() >= 2);
    }

    /// Required test 7: every iteration reaches the audit and the export.
    #[tokio::test]
    async fn every_iteration_appears_in_the_audit_and_the_evidence_export() {
        let harness = Harness::new("review-evidence");
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[FIX, PASS]);
        let worker = ScriptedWorker::new(vec![Ok(())]);
        let mut verification = harness.passing_verification();
        let mut all = verification.clone();

        harness
            .review_loop(&critic, &worker, &[])
            .run(&harness.milestone, &mut verification, &mut all)
            .await
            .unwrap()
            .unwrap();

        let snapshot = harness
            .state
            .manager
            .evidence_snapshot(harness.task.id)
            .unwrap();
        let files = crate::evidence::export(&snapshot).unwrap().files;
        let file = |name: &str| {
            files
                .iter()
                .find(|(entry, _)| entry == name)
                .unwrap()
                .1
                .clone()
        };
        let jsonl = file(crate::evidence::JSONL_FILENAME);
        for kind in [
            "implementation_review_started",
            "implementation_review_completed",
            "fix_started",
            "fix_completed",
        ] {
            assert!(jsonl.contains(kind), "{kind} missing from the JSONL export");
        }
        assert_eq!(
            jsonl.matches("implementation_review_completed").count(),
            2,
            "both review rounds must be exported"
        );
        assert!(
            jsonl.contains("add the invoice module"),
            "findings exported"
        );
        let log = file(crate::evidence::DEVELOPMENT_LOG_FILENAME);
        assert!(log.contains("Implementation review completed for milestone 1"));
        assert!(log.contains("Review fix iteration 1 completed for milestone 1"));
        assert!(log.contains("Stage: fix"), "the fix run is staged as a fix");
        assert!(log.contains("Stage: implementation review"));
        let report = file(crate::evidence::FINAL_REPORT_FILENAME);
        assert!(
            report.contains("Implementation review: PASS after 1 of 2 fix iteration(s)"),
            "{report}"
        );
    }

    /// Required test 8: the review and the fix use the models the task was
    /// created with, not the current defaults.
    #[tokio::test]
    async fn the_configured_critic_and_worker_are_used() {
        let harness = Harness::new("review-models");
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[FIX, PASS]);
        let worker = ScriptedWorker::new(vec![Ok(())]);
        let mut verification = harness.passing_verification();
        let mut all = verification.clone();

        harness
            .review_loop(&critic, &worker, &[])
            .run(&harness.milestone, &mut verification, &mut all)
            .await
            .unwrap()
            .unwrap();

        let stored = harness.stored();
        let review_models = stored
            .history
            .iter()
            .filter_map(|recorded| match &recorded.event {
                TaskEvent::ImplementationReviewStarted {
                    provider, model, ..
                } => Some((*provider, model.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(review_models.len(), 2);
        for (provider, model) in review_models {
            assert_eq!(provider, ChatProvider::Anthropic);
            assert_eq!(model, "configured-critic-model");
        }
        let fix_models = stored
            .history
            .iter()
            .filter_map(|recorded| match &recorded.event {
                TaskEvent::FixStarted { tool, model, .. }
                | TaskEvent::FixCompleted { tool, model, .. } => Some((*tool, model.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(fix_models.len(), 2);
        for (tool, model) in fix_models {
            assert_eq!(tool, CodingTool::ClaudeCode);
            assert_eq!(model, "configured-worker-model");
        }
        // The retained evidence names the same agents.
        let evidence = serde_json::to_string(&stored.evidence).unwrap();
        assert!(evidence.contains("configured-critic-model"), "{evidence}");
        assert!(evidence.contains("configured-worker-model"), "{evidence}");
        assert!(evidence.contains("implementation_review"), "{evidence}");
        assert!(evidence.contains("\"stage\":\"fix\""), "{evidence}");
    }

    /// Cancellation during the fix loop stops without failing or completing.
    #[tokio::test]
    async fn cancellation_stops_the_loop_without_claiming_either_outcome() {
        let harness = Harness::new("review-cancelled");
        let critic = ScriptedAgent::new(ChatProvider::Anthropic, &[PASS]);
        let worker = ScriptedWorker::new(vec![]);
        let mut verification = harness.passing_verification();
        let mut all = verification.clone();
        assert!(harness.state.manager.cancel(harness.task.id));

        let review = harness
            .review_loop(&critic, &worker, &[])
            .run(&harness.milestone, &mut verification, &mut all)
            .await
            .unwrap();

        assert!(review.is_none());
        assert_eq!(critic.calls(), 0);
        let kinds = harness.event_kinds();
        assert!(kinds.contains(&"milestone_cancelled".to_string()));
        assert!(!kinds.contains(&"milestone_failed".to_string()));
        assert!(!kinds.contains(&"milestone_completed".to_string()));
    }
}
