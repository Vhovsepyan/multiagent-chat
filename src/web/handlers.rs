//! The four REST endpoints from plan_v2 Phase 8, plus a health check.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::target;
use crate::task::{Decision, Task, TaskId, TaskStatus};
use crate::web::{AppState, pipeline};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// An API failure, rendered as JSON rather than a bare status code.
///
/// `IntoResponse` is what lets a handler return `Result<_, ApiError>` and have
/// axum turn the error arm into a real HTTP response.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn not_found(message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    /// The request made sense but the task is in the wrong state for it.
    pub fn conflict(message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

type ApiResult<T> = std::result::Result<T, ApiError>;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
}

pub async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Serialize)]
pub struct ProjectList {
    pub projects: Vec<String>,
}

/// `GET /api/projects` — the folders inside WORKSPACE_ROOT.
pub async fn list_projects(State(state): State<AppState>) -> ApiResult<Json<ProjectList>> {
    let projects =
        target::list_projects(&state.config).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(ProjectList { projects }))
}

#[derive(Debug, Deserialize)]
pub struct CreateTask {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub project: String,
}

/// `POST /api/tasks` — create a task and start the pipeline in the background.
///
/// Returns 201 immediately; the work continues on a spawned tokio task and is
/// followed via `GET /api/tasks/{id}` (and, from Phase 9, the SSE stream).
pub async fn create_task(
    State(state): State<AppState>,
    Json(body): Json<CreateTask>,
) -> ApiResult<(StatusCode, Json<Task>)> {
    if body.title.trim().is_empty() {
        return Err(ApiError::bad_request("title cannot be empty"));
    }
    if body.project.trim().is_empty() {
        return Err(ApiError::bad_request("project cannot be empty"));
    }

    // Resolve the project up front so a bad name fails the request, rather than
    // failing invisibly inside the background task a moment later.
    let repo = target::ensure_project(&state.config, &body.project)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let task = state.manager.create(
        body.title.trim(),
        body.description.trim(),
        body.project.trim(),
    );

    pipeline::spawn(state.clone(), task.id, repo);

    Ok((StatusCode::CREATED, Json(task)))
}

/// `GET /api/tasks/{id}` — the current snapshot, including full event history.
pub async fn get_task(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
) -> ApiResult<Json<Task>> {
    state
        .manager
        .get(id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("no task {id}")))
}

/// `POST /api/tasks/{id}/approve` — Gate 2 (DP-10).
///
/// The body may carry an edited spec, which replaces SPEC.md before the build.
pub async fn approve_task(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
    Json(decision): Json<Decision>,
) -> ApiResult<Json<Task>> {
    let task = state
        .manager
        .get(id)
        .ok_or_else(|| ApiError::not_found(format!("no task {id}")))?;

    if task.status != TaskStatus::WaitingForApproval {
        return Err(ApiError::conflict(format!(
            "task is {:?}, not waiting for approval",
            task.status
        )));
    }
    if let Some(spec) = &decision.spec
        && spec.trim().is_empty()
    {
        return Err(ApiError::bad_request("an edited spec cannot be empty"));
    }

    if !state.manager.decide(id, decision) {
        return Err(ApiError::not_found(format!("no task {id}")));
    }

    let updated = state
        .manager
        .get(id)
        .ok_or_else(|| ApiError::internal("task vanished while approving"))?;
    Ok(Json(updated))
}
