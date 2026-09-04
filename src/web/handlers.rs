//! The four REST endpoints from plan_v2 Phase 8, plus a health check.

use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};

use crate::project::{Project, ProjectSource};
use crate::task::{Decision, Task, TaskId, TaskRequest, TaskStatus};
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
    pub projects: Vec<Project>,
}

/// `GET /api/projects` — registered repository-backed Projects.
pub async fn list_projects(State(state): State<AppState>) -> ApiResult<Json<ProjectList>> {
    Ok(Json(ProjectList {
        projects: state.projects.list(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct RegisterProject {
    pub name: String,
    pub repository: String,
    #[serde(default = "default_branch")]
    pub default_branch: String,
}

fn default_branch() -> String {
    "main".into()
}

pub async fn register_project(
    State(state): State<AppState>,
    Json(body): Json<RegisterProject>,
) -> ApiResult<(StatusCode, Json<Project>)> {
    let source = ProjectSource::github(&body.repository)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let project = Project::new(&body.name, source, &body.default_branch)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let project = state
        .projects
        .register(project)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok((StatusCode::CREATED, Json(project)))
}

#[derive(Debug, Deserialize)]
pub struct CreateTask {
    #[serde(flatten)]
    pub request: TaskRequest,
}

/// `POST /api/tasks` — create a task and start the pipeline in the background.
///
/// Returns 201 immediately; the work continues on a spawned tokio task and is
/// followed via `GET /api/tasks/{id}` (and, from Phase 9, the SSE stream).
pub async fn create_task(
    State(state): State<AppState>,
    Json(body): Json<CreateTask>,
) -> ApiResult<(StatusCode, Json<Task>)> {
    body.request.validate().map_err(ApiError::bad_request)?;
    if let Some(project_id) = body.request.project_id
        && state.projects.get(project_id).is_none()
    {
        return Err(ApiError::bad_request("project is not registered"));
    }

    let task = state
        .manager
        .create_from_request(body.request)
        .map_err(ApiError::bad_request)?;

    pipeline::spawn(state.clone(), task.id);

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

/// `GET /api/tasks/{id}/events` — live `TaskEvent`s as Server-Sent Events.
///
/// Each message is one JSON-encoded `TaskEvent`, so the browser can
/// `JSON.parse(e.data)` and switch on `type`.
///
/// This streams events from the moment you connect. The full backlog lives on
/// `GET /api/tasks/{id}` as `history`, so the client fetches the snapshot first
/// and then subscribes — see the note in `task.rs`.
pub async fn task_events(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
) -> ApiResult<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>> {
    if state.manager.get(id).is_none() {
        return Err(ApiError::not_found(format!("no task {id}")));
    }

    // Subscribe before returning, so nothing emitted while the response is
    // being set up is lost.
    let stream = BroadcastStream::new(state.manager.subscribe()).filter_map(move |received| {
        match received {
            // Every task shares one channel, so filter to the one asked for.
            Ok((event_id, event)) if event_id == id => Some(Ok(Event::default()
                .json_data(&event)
                .unwrap_or_else(|_| Event::default().data("{}")))),
            Ok(_) => None,
            // The client fell more than EVENT_BUFFER events behind. Say so
            // rather than silently skipping: the UI should re-fetch the
            // snapshot instead of showing a debate with holes in it.
            Err(BroadcastStreamRecvError::Lagged(missed)) => Some(Ok(Event::default()
                .event("lagged")
                .data(missed.to_string()))),
        }
    });

    // The keep-alive comment stops idle proxies and browsers dropping a
    // connection during a long silent stretch, such as a slow build.
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
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
