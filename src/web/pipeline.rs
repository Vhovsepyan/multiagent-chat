//! Repository-backed orchestration in a disposable task workspace.

use anyhow::{Result, bail};

use crate::api::claude::ClaudeClient;
use crate::api::gemini::GeminiClient;
use crate::inspection::{InspectionRequest, inspect};
use crate::project::Project;
use crate::spec;
use crate::task::{Emitter, TaskEvent, TaskId, TaskKind, TaskResult, TaskStatus};
use crate::technology::ProjectProfile;
use crate::web::AppState;
use crate::workspace::{TaskWorkspace, WorkspaceRequest, diff_result};

pub fn spawn(state: AppState, id: TaskId) {
    tokio::spawn(async move {
        let emitter = state.manager.emitter(id);
        let mut workspace = None;
        let result = run(&state, id, &emitter, &mut workspace).await;

        if let Some(workspace) = workspace
            && let Err(error) = state.workspaces.cleanup(&workspace)
        {
            emitter.warn(format!("workspace cleanup failed: {error}"));
        }

        if let Err(error) = result {
            emitter.emit(TaskEvent::Finished {
                status: TaskStatus::Failed,
                error: Some(format!("{error:#}")),
            });
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
            let prepared = prepare_existing(state, id, project)?;
            let inspection = inspect(
                &prepared.path,
                InspectionRequest {
                    kind: task.kind,
                    title: &task.title,
                    description: &task.description,
                },
            )?;
            let profile = inspection.profile.clone();
            state.projects.set_profile(project.id, profile.clone());
            let context = inspection.prompt_context();
            emitter.emit(TaskEvent::Inspection {
                profile: profile.clone(),
                source_revision: prepared.revision.clone(),
            });
            *workspace = Some(prepared);
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
    let proposer = GeminiClient::new(&state.config)?;
    let critic = ClaudeClient::new(&state.config)?;

    emitter.status(TaskStatus::Debating);
    let outcome =
        crate::debate::run(&proposer, &critic, &topic, state.config.max_rounds, emitter).await?;

    emitter.status(TaskStatus::GeneratingSpec);
    let document = spec::build(
        &proposer,
        &critic,
        &outcome.transcript,
        outcome.approved,
        emitter,
    )
    .await?;
    emitter.emit(TaskEvent::Spec {
        markdown: document,
        path: spec::SPEC_FILENAME.into(),
    });

    emitter.status(TaskStatus::WaitingForApproval);
    let Some(decision) = state.manager.await_decision(id).await else {
        return Ok(());
    };
    if !decision.approve {
        emitter.notice("rejected; no repository changes were published");
        emitter.emit(TaskEvent::Finished {
            status: TaskStatus::Rejected,
            error: None,
        });
        return Ok(());
    }

    if workspace.is_none() {
        *workspace = Some(state.workspaces.prepare(WorkspaceRequest {
            task_id: id,
            source: None,
            revision: None,
        })?);
    }
    let workspace_ref = workspace.as_ref().expect("workspace was prepared");
    let approved_spec = decision
        .spec
        .or_else(|| state.manager.get(id).and_then(|task| task.spec))
        .expect("generated specification is stored");
    spec::write_to(&workspace_ref.path, &approved_spec)?;

    emitter.status(TaskStatus::Implementing);
    let prompt = crate::workflow::implementation_prompt(task.kind, &profile);
    crate::implementer::run_with_prompt(&state.config, &workspace_ref.path, emitter, &prompt)
        .await?;

    let commands = crate::verification::plan(&profile, &workspace_ref.path);
    if commands.is_empty() {
        emitter.warn("no automatic verification commands were detected");
    }
    let verification = crate::verification::run(&commands, &workspace_ref.path).await?;
    for result in &verification {
        emitter.emit(TaskEvent::Verification {
            result: result.clone(),
        });
    }

    let failed = verification.iter().any(|result| !result.success);
    let result = TaskResult {
        source_revision: workspace_ref.revision.clone(),
        verification,
        diff: diff_result(&workspace_ref.path)?,
    };
    emitter.emit(TaskEvent::Result { result });
    if failed {
        bail!("one or more verification commands failed");
    }

    emitter.emit(TaskEvent::Finished {
        status: TaskStatus::Completed,
        error: None,
    });
    Ok(())
}

fn prepare_existing(state: &AppState, id: TaskId, project: &Project) -> Result<TaskWorkspace> {
    state.workspaces.prepare(WorkspaceRequest {
        task_id: id,
        source: Some(&project.source),
        revision: Some(&project.default_branch),
    })
}
