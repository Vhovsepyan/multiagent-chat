//! Server-rendered production UI and HTML SSE fragments.

use std::convert::Infallible;

use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};

use crate::project::{Project, ProjectSource};
use crate::task::{
    Decision, OutputTarget, Task, TaskEvent, TaskId, TaskKind, TaskRequest, TaskStatus,
};
use crate::technology::TechStack;
use crate::web::{AppState, pipeline};

fn esc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn timeline_html(status: TaskStatus) -> String {
    let steps = [
        ("Debate", TaskStatus::Debating),
        ("Spec", TaskStatus::GeneratingSpec),
        ("Approval", TaskStatus::WaitingForApproval),
        ("Build", TaskStatus::Implementing),
    ];
    let rank = |value| match value {
        TaskStatus::Created => 0,
        TaskStatus::Debating => 1,
        TaskStatus::GeneratingSpec => 2,
        TaskStatus::WaitingForApproval => 3,
        TaskStatus::Implementing => 4,
        TaskStatus::Completed | TaskStatus::Rejected | TaskStatus::Failed => 5,
    };
    let mut html = String::from(r#"<div class="timeline">"#);
    for (label, step) in steps {
        let class = if rank(step) < rank(status) {
            "step done"
        } else if step == status {
            "step active"
        } else {
            "step"
        };
        html.push_str(&format!(r#"<span class="{class}">{label}</span>"#));
    }
    let (class, label) = match status {
        TaskStatus::Completed => ("step done", "Completed"),
        TaskStatus::Rejected => ("step bad", "Rejected"),
        TaskStatus::Failed => ("step bad", "Failed"),
        _ => ("step", "Done"),
    };
    html.push_str(&format!(r#"<span class="{class}">{label}</span></div>"#));
    html
}

fn gate_html(id: TaskId, spec: &str) -> String {
    format!(
        r#"<div class="card"><h2 class="section">SPEC.md — your call</h2>
<form hx-post="/ui/tasks/{id}/approve" hx-swap="none">
<textarea class="spec-edit" name="spec">{}</textarea>
<div class="hint">Edit freely — the approved text is what gets built.</div>
<button type="submit" name="approve" value="true">Approve &amp; Build</button>
<button type="submit" name="approve" value="false" class="danger">Reject</button>
</form></div>"#,
        esc(spec)
    )
}

fn event_html(id: TaskId, event: &TaskEvent) -> Option<(&'static str, String)> {
    match event {
        TaskEvent::Status { status } => Some(("status", timeline_html(*status))),
        TaskEvent::RoundStarted { round, of } => Some((
            "debate",
            format!(r#"<h2 class="section">Round {round} of {of}</h2>"#),
        )),
        TaskEvent::Proposal { text, .. } => Some((
            "debate",
            format!(
                r#"<div class="turn proposer"><h3>Proposer · Gemini</h3><pre>{}</pre></div>"#,
                esc(text)
            ),
        )),
        TaskEvent::Critique {
            text,
            verdict,
            reason,
            ..
        } => {
            let badge = verdict
                .as_deref()
                .map(|value| {
                    let label = if value == "approved" {
                        "VERDICT: APPROVED"
                    } else {
                        "VERDICT: NEEDS_WORK"
                    };
                    format!(
                        r#"<div class="verdict {value}">{label} {}</div>"#,
                        esc(reason.as_deref().unwrap_or(""))
                    )
                })
                .unwrap_or_default();
            Some((
                "debate",
                format!(
                    r#"<div class="turn critic"><h3>Critic · Claude</h3><pre>{}</pre>{badge}</div>"#,
                    esc(text)
                ),
            ))
        }
        TaskEvent::Spec { markdown, .. } => Some(("spec", gate_html(id, markdown))),
        TaskEvent::SpecApproved { markdown } => Some((
            "spec",
            format!(
                r#"<div class="card"><h2 class="section">SPEC.md</h2><div class="spec-body">{}</div></div>"#,
                esc(markdown)
            ),
        )),
        TaskEvent::Inspection {
            profile,
            source_revision,
        } => Some((
            "debate",
            format!(
                r#"<div class="notice">Detected <strong>{}</strong>{}</div>"#,
                esc(&format!("{:?}", profile.stack)),
                source_revision
                    .as_deref()
                    .map(|revision| format!(" at <code>{}</code>", esc(revision)))
                    .unwrap_or_default()
            ),
        )),
        TaskEvent::Verification { result } => Some((
            "build",
            format!(
                r#"<div>{}: {}</div>"#,
                esc(&result.command),
                if result.success { "passed" } else { "failed" }
            ),
        )),
        TaskEvent::Result { result } => {
            Some(("build", format!("<pre>{}</pre>", esc(&result.diff))))
        }
        TaskEvent::Build { chunk } => Some(("build", format!("<div>{}</div>", esc(chunk)))),
        TaskEvent::Notice { message } => Some((
            "debate",
            format!(r#"<div class="notice">{}</div>"#, esc(message)),
        )),
        TaskEvent::Warning { message } => Some((
            "debate",
            format!(r#"<div class="notice warn">{}</div>"#, esc(message)),
        )),
        TaskEvent::Finished { status, error } => {
            let (class, text) = match status {
                TaskStatus::Completed => (
                    "ok",
                    "Completed — implementation and verification finished.".into(),
                ),
                TaskStatus::Rejected => (
                    "warn",
                    "Rejected. No repository changes were published.".into(),
                ),
                _ => (
                    "bad",
                    format!("Failed: {}", esc(error.as_deref().unwrap_or("unknown"))),
                ),
            };
            Some((
                "done",
                format!(r#"<div class="done-banner {class}">{text}</div>"#),
            ))
        }
    }
}

pub async fn projects(State(state): State<AppState>) -> Html<String> {
    let projects = state.projects.list();
    let mut html = String::new();
    if projects.is_empty() {
        html.push_str(r#"<option value="">no repositories registered yet</option>"#);
    }
    for project in projects {
        html.push_str(&format!(
            r#"<option value="{}">{}</option>"#,
            project.id,
            esc(&project.name)
        ));
    }
    Html(html)
}

#[derive(Debug, Deserialize)]
pub struct RegisterProjectForm {
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
    Form(form): Form<RegisterProjectForm>,
) -> Response {
    let result = ProjectSource::github(&form.repository)
        .and_then(|source| Project::new(&form.name, source, &form.default_branch))
        .and_then(|project| state.projects.register(project));
    match result {
        Ok(project) => Html(format!(
            r#"<div class="notice ok">Registered {}. Reload the project list to select it.</div>"#,
            esc(&project.name)
        ))
        .into_response(),
        Err(error) => error_fragment(&error.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateForm {
    pub kind: TaskKind,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub project_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub technology: Option<TechStack>,
    #[serde(default)]
    pub output: Option<OutputTarget>,
}

pub async fn create(State(state): State<AppState>, Form(form): Form<CreateForm>) -> Response {
    let request = TaskRequest {
        kind: form.kind,
        title: form.title,
        description: form.description,
        project_id: form.project_id,
        technology: form.technology,
        output: form.output,
    };
    if let Err(error) = request.validate() {
        return error_fragment(&error);
    }
    if let Some(project_id) = request.project_id
        && state.projects.get(project_id).is_none()
    {
        return error_fragment("Select a registered project.");
    }
    let task = match state.manager.create_from_request(request) {
        Ok(task) => task,
        Err(error) => return error_fragment(&error),
    };
    pipeline::spawn(state, task.id);
    let mut headers = HeaderMap::new();
    headers.insert(
        "HX-Redirect",
        format!("/task/{}", task.id).parse().expect("valid header"),
    );
    (headers, Html(String::new())).into_response()
}

fn error_fragment(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(format!(
            r#"<div class="notice err" id="create-error">{}</div>"#,
            esc(message)
        )),
    )
        .into_response()
}

pub async fn task_page(State(state): State<AppState>, Path(id): Path<TaskId>) -> Response {
    let Some(task) = state.manager.get(id) else {
        return (StatusCode::NOT_FOUND, Html("<h1>No such task</h1>")).into_response();
    };
    let mut debate = String::new();
    let mut spec = String::new();
    let mut build = String::new();
    let mut done = String::new();
    for event in &task.history {
        if let Some((slot, html)) = event_html(id, event) {
            match slot {
                "debate" => debate.push_str(&html),
                "spec" => spec = html,
                "build" => build.push_str(&html),
                "done" => done = html,
                _ => {}
            }
        }
    }
    if task.status != TaskStatus::WaitingForApproval {
        spec = spec_readonly(&task);
    }
    let project_name = task
        .project_id
        .and_then(|project_id| state.projects.get(project_id))
        .map(|project| project.name)
        .unwrap_or_else(|| "New project".into());
    Html(page_html(
        &task,
        &project_name,
        &debate,
        &spec,
        &build,
        &done,
    ))
    .into_response()
}

fn spec_readonly(task: &Task) -> String {
    task.spec.as_ref().map(|spec| format!(r#"<div class="card"><h2 class="section">SPEC.md</h2><div class="spec-body">{}</div></div>"#, esc(spec))).unwrap_or_default()
}

fn page_html(
    task: &Task,
    project: &str,
    debate: &str,
    spec: &str,
    build: &str,
    done: &str,
) -> String {
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title} — multiagent-chat</title><link rel="stylesheet" href="/static/style.css"><script src="/static/vendor/htmx.min.js"></script><script src="/static/vendor/sse.js"></script></head><body><div class="wrap" hx-ext="sse" sse-connect="/ui/tasks/{id}/stream"><header class="top"><h1>{title}</h1><span class="sub"><a href="/">&larr; new task</a> · {kind} · <code>{project}</code></span></header><div id="timeline" sse-swap="status" hx-swap="innerHTML">{timeline}</div><div id="done" sse-swap="done" hx-swap="innerHTML">{done}</div><div id="spec" sse-swap="spec" hx-swap="innerHTML">{spec}</div><h2 class="section">Debate</h2><div id="debate" sse-swap="debate" hx-swap="beforeend">{debate}</div><h2 class="section">Implementation / Verification / Result</h2><div id="terminal" class="terminal" sse-swap="build" hx-swap="beforeend">{build}</div></div></body></html>"##,
        id = task.id,
        title = esc(&task.title),
        kind = task.kind.label(),
        project = esc(project),
        timeline = timeline_html(task.status)
    )
}

pub async fn stream(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let events = BroadcastStream::new(state.manager.subscribe()).filter_map(move |received| match received {
        Ok((event_id, event)) if event_id == id => event_html(id, &event).map(|(name, html)| Ok(Event::default().event(name).data(html))),
        Ok(_) => None,
        Err(BroadcastStreamRecvError::Lagged(_)) => Some(Ok(Event::default().event("debate").data(r#"<div class="notice warn">Some output was dropped — reload to catch up.</div>"#))),
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

#[derive(Debug, Deserialize)]
pub struct ApproveForm {
    pub approve: String,
    #[serde(default)]
    pub spec: Option<String>,
}

pub async fn approve(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
    Form(form): Form<ApproveForm>,
) -> Response {
    let approve = form.approve == "true";
    let spec = match (&form.spec, approve) {
        (Some(text), true) if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    };
    if !state.manager.decide(id, Decision { approve, spec }) {
        return (StatusCode::NOT_FOUND, Html("unknown task")).into_response();
    }
    let body = state
        .manager
        .get(id)
        .map(|task| spec_readonly(&task))
        .unwrap_or_default();
    Html(body).into_response()
}
