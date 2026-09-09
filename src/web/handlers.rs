//! The four REST endpoints from plan_v2 Phase 8, plus a health check.

use std::convert::Infallible;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};

use crate::agent::{AgentSelection, ModelOptions};
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
// JSON body extraction
// ---------------------------------------------------------------------------

/// `Json<T>`, but a malformed or unsupported body is reported the way every
/// other client error in this API is: 400 with `{"error": "..."}`.
///
/// Plain `Json<T>` answers a bad enum variant with a 422 and a framework
/// sentence, so an unsupported provider looked like a different class of
/// failure from an unsupported model. Both are the client asking for something
/// this installation does not serve.
pub struct ValidJson<T>(pub T);

impl<S, T> FromRequest<S> for ValidJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> std::result::Result<Self, ApiError> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(ValidJson(value)),
            Err(rejection) => Err(ApiError::bad_request(readable_json_error(&rejection))),
        }
    }
}

/// Keep serde's useful part — the field path and what was expected — and drop
/// the framework preamble and the byte offset.
fn readable_json_error(rejection: &JsonRejection) -> String {
    const PREFIXES: [&str; 2] = [
        "Failed to deserialize the JSON body into the target type: ",
        "Failed to parse the request body as JSON: ",
    ];
    let text = rejection.body_text();
    let mut detail = text.as_str();
    for prefix in PREFIXES {
        if let Some(rest) = detail.strip_prefix(prefix) {
            detail = rest;
        }
    }
    // serde appends "at line 1 column 173", which locates a byte, not a field.
    let detail = detail
        .split(" at line ")
        .next()
        .unwrap_or(detail)
        .trim()
        .trim_end_matches('.');
    format!("invalid request body: {detail}")
}

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

/// The safe view of agent configuration (task 0005, requirement 10).
///
/// Names only: no keys, no credentials, no raw config. Everything here is
/// already public knowledge for anyone who can open the task form.
#[derive(Serialize)]
pub struct AgentOptions {
    /// Only providers this installation has credentials for.
    pub chat_providers: Vec<ModelOptions>,
    pub coding_tools: Vec<ModelOptions>,
    /// `None` when a default role has no available provider; `unavailable`
    /// then says which variable to set. Neither field carries a credential.
    pub defaults: Option<AgentSelection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// `GET /api/agents` — what the task form may offer.
pub async fn agent_options(State(state): State<AppState>) -> Json<AgentOptions> {
    let (defaults, unavailable) = match state.catalogue.defaults() {
        Ok(defaults) => (Some(defaults), None),
        Err(error) => (None, Some(error)),
    };
    Json(AgentOptions {
        chat_providers: state.catalogue.available_chat_providers(),
        coding_tools: state.catalogue.available_coding_tools(),
        defaults,
        unavailable,
    })
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
    ValidJson(body): ValidJson<RegisterProject>,
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

/// `POST /api/tasks` — create a task and start the pipeline in the background.
///
/// Returns 201 immediately; the work continues on a spawned tokio task and is
/// followed via `GET /api/tasks/{id}` (and, from Phase 9, the SSE stream).
pub async fn create_task(
    State(state): State<AppState>,
    ValidJson(request): ValidJson<TaskRequest>,
) -> ApiResult<(StatusCode, Json<Task>)> {
    request.validate().map_err(ApiError::bad_request)?;
    if let Some(project_id) = request.project_id
        && state.projects.get(project_id).is_none()
    {
        return Err(ApiError::bad_request("project is not registered"));
    }

    // Task 0005: resolve and freeze the agent selection before the task exists,
    // so an invalid combination is a 400 rather than a task that fails later.
    let agents = state
        .catalogue
        .resolve(request.agents.as_ref())
        .map_err(ApiError::bad_request)?;

    let task = state
        .manager
        .create_from_request(request, agents)
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

/// `GET /api/tasks/{id}/evidence` — generate the five-file evidence package.
///
/// The archive is rendered in memory from an application-owned task snapshot;
/// no title or caller-supplied path participates in filesystem access.
pub async fn export_evidence(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
) -> ApiResult<Response> {
    let task = state
        .manager
        .evidence_snapshot(id)
        .ok_or_else(|| ApiError::not_found(format!("no task {id}")))?;
    let package = tokio::task::spawn_blocking(move || crate::evidence::export(&task))
        .await
        .map_err(|error| ApiError::internal(format!("evidence export task failed: {error}")))?
        .map_err(|error| ApiError::internal(format!("could not export evidence: {error:#}")))?;
    // Only a generated archive is an export: recording earlier would leave a
    // successful-looking audit event behind a failed download.
    state.manager.record_evidence_export(id);
    let disposition =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", package.filename))
            .map_err(|_| ApiError::internal("could not create evidence download filename"))?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/zip"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        package.bytes,
    )
        .into_response())
}

/// `GET /api/tasks/{id}/events` — live `TaskEvent`s as Server-Sent Events.
///
/// Each message is one JSON-encoded `RecordedEvent`; its nested `event` keeps
/// the tagged `TaskEvent` payload while sequence and timestamp are assigned by
/// the backend.
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
/// The body may carry an edited spec, used for the external artifact before build.
pub async fn approve_task(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
    ValidJson(decision): ValidJson<Decision>,
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

    state
        .manager
        .decide_checked(id, decision)
        .map_err(|error| match error {
            crate::task::DecisionError::NotFound => ApiError::not_found(error.to_string()),
            crate::task::DecisionError::NotWaiting => ApiError::conflict(error.to_string()),
            crate::task::DecisionError::InvalidSpec => ApiError::bad_request(error.to_string()),
        })?;

    let updated = state
        .manager
        .get(id)
        .ok_or_else(|| ApiError::internal("task vanished while approving"))?;
    Ok(Json(updated))
}
