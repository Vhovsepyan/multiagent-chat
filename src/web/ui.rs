//! Phase 10: the HTML the browser actually renders.
//!
//! DP-14 chose HTMX, which swaps HTML fragments rather than consuming JSON. The
//! `/api/*` endpoints stay exactly as they are — still JSON, still tested, still
//! usable from curl — and everything the UI needs is rendered here under `/ui`.
//! Keeping the two apart means the API contract is never bent to suit a widget.
//!
//! The task page is server-rendered rather than static because it has to embed
//! the task id and replay `history` before subscribing. Rendering the backlog
//! server-side also closes the snapshot-then-subscribe race noted in `task.rs`:
//! the page is built and the SSE stream attached in one response.

use axum::Form;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};

use crate::target;
use crate::task::{Decision, Task, TaskEvent, TaskId, TaskStatus};
use crate::web::{AppState, pipeline};

// ---------------------------------------------------------------------------
// Escaping
// ---------------------------------------------------------------------------

/// Escape text before it goes into HTML.
///
/// Everything the models write lands on this page, and a proposal that mentions
/// `<script>` or a stray `&` must render as text, not run or corrupt the markup.
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

// ---------------------------------------------------------------------------
// Fragments
// ---------------------------------------------------------------------------

/// The pipeline timeline, re-rendered on every status change.
fn timeline_html(status: TaskStatus) -> String {
    // (label, the status this step represents)
    let steps = [
        ("Debate", TaskStatus::Debating),
        ("Spec", TaskStatus::GeneratingSpec),
        ("Approval", TaskStatus::WaitingForApproval),
        ("Build", TaskStatus::Implementing),
    ];
    let rank = |s: TaskStatus| match s {
        TaskStatus::Created => 0,
        TaskStatus::Debating => 1,
        TaskStatus::GeneratingSpec => 2,
        TaskStatus::WaitingForApproval => 3,
        TaskStatus::Implementing => 4,
        TaskStatus::Completed | TaskStatus::Rejected | TaskStatus::Failed => 5,
    };
    let here = rank(status);

    let mut html = String::from(r#"<div class="timeline">"#);
    for (label, step) in steps {
        let at = rank(step);
        let class = if status == TaskStatus::Failed && at >= here {
            "step bad"
        } else if at < here {
            "step done"
        } else if at == here {
            "step active"
        } else {
            "step"
        };
        html.push_str(&format!(r#"<span class="{class}">{label}</span>"#));
    }
    let (final_class, final_label) = match status {
        TaskStatus::Completed => ("step done", "Completed"),
        TaskStatus::Rejected => ("step bad", "Rejected"),
        TaskStatus::Failed => ("step bad", "Failed"),
        _ => ("step", "Done"),
    };
    html.push_str(&format!(
        r#"<span class="{final_class}">{final_label}</span></div>"#
    ));
    html
}

/// The Approve / Edit / Reject panel, shown only at Gate 2.
fn gate_html(id: TaskId, spec: &str) -> String {
    format!(
        r#"<div class="card">
  <h2 class="section">SPEC.md — your call</h2>
  <form hx-post="/ui/tasks/{id}/approve" hx-swap="none">
    <textarea class="spec-edit" name="spec">{spec}</textarea>
    <div class="hint">Edit freely — what is in the box is what gets built.</div>
    <button type="submit" name="approve" value="true">Approve &amp; Build</button>
    <button type="submit" name="approve" value="false" class="danger">Reject</button>
  </form>
</div>"#,
        spec = esc(spec)
    )
}

/// Render one event as the fragment its target div expects.
///
/// Returns `(sse_event_name, html)`. The name decides which `sse-swap` on the
/// page receives it, which is how one stream feeds four different regions.
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
            let badge = match verdict.as_deref() {
                Some(v) => {
                    let label = if v == "approved" {
                        "VERDICT: APPROVED"
                    } else {
                        "VERDICT: NEEDS_WORK"
                    };
                    let why = reason
                        .as_deref()
                        .map(|r| format!(r#" <span class="reason">— {}</span>"#, esc(r)))
                        .unwrap_or_default();
                    format!(r#"<div class="verdict {v}">{label}{why}</div>"#)
                }
                None => String::new(),
            };
            Some((
                "debate",
                format!(
                    r#"<div class="turn critic"><h3>Critic · Claude</h3><pre>{}</pre>{badge}</div>"#,
                    esc(text)
                ),
            ))
        }

        TaskEvent::Spec { markdown, .. } => Some(("spec", gate_html(id, markdown))),

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
                TaskStatus::Completed => ("ok", "Completed — Claude Code finished.".to_string()),
                TaskStatus::Rejected => (
                    "warn",
                    "Rejected. SPEC.md is on disk if you want to edit it and re-run.".to_string(),
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

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// `GET /ui/projects` — `<option>` list for the picker.
pub async fn projects(State(state): State<AppState>) -> Html<String> {
    let names = target::list_projects(&state.config).unwrap_or_default();
    let mut html = String::new();
    if names.is_empty() {
        html.push_str(r#"<option value="">no projects yet — name a new one below</option>"#);
    }
    for name in names {
        html.push_str(&format!(
            r#"<option value="{n}">{n}</option>"#,
            n = esc(&name)
        ));
    }
    Html(html)
}

#[derive(Debug, Deserialize)]
pub struct CreateForm {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub new_project: String,
}

/// `POST /ui/tasks` — the create form.
///
/// Answers with `HX-Redirect`, which tells HTMX to navigate the whole window to
/// the task page rather than swapping a fragment into the form.
pub async fn create(State(state): State<AppState>, Form(form): Form<CreateForm>) -> Response {
    // A typed new project wins over the dropdown selection.
    let project = if form.new_project.trim().is_empty() {
        form.project.trim()
    } else {
        form.new_project.trim()
    };

    if form.title.trim().is_empty() {
        return error_fragment("A title is required.");
    }
    if project.is_empty() {
        return error_fragment("Pick a project or name a new one.");
    }

    let repo = match target::ensure_project(&state.config, project) {
        Ok(repo) => repo,
        Err(e) => return error_fragment(&e.to_string()),
    };

    let task = state
        .manager
        .create(form.title.trim(), form.description.trim(), project);
    pipeline::spawn(state.clone(), task.id, repo);

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
            r#"<div class="notice err" id="create-error" hx-swap-oob="true">{}</div>"#,
            esc(message)
        )),
    )
        .into_response()
}

/// `GET /task/{id}` — Mission Control.
///
/// History is rendered inline before the stream is attached, so a page opened
/// mid-run shows everything that already happened.
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
    // The gate panel only belongs on the page while it is actually open.
    if task.status != TaskStatus::WaitingForApproval {
        spec = spec_readonly(&task);
    }

    Html(page_html(
        &task,
        &timeline_html(task.status),
        &debate,
        &spec,
        &build,
        &done,
    ))
    .into_response()
}

/// Once the gate is answered the spec is shown, but no longer editable.
fn spec_readonly(task: &Task) -> String {
    match &task.spec {
        Some(spec) => format!(
            r#"<div class="card"><h2 class="section">SPEC.md</h2><div class="spec-body">{}</div></div>"#,
            esc(spec)
        ),
        None => String::new(),
    }
}

fn page_html(
    task: &Task,
    timeline: &str,
    debate: &str,
    spec: &str,
    build: &str,
    done: &str,
) -> String {
    let id = task.id;
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — multiagent-chat</title>
<link rel="stylesheet" href="/static/style.css">
<script src="/static/vendor/htmx.min.js"></script>
<script src="/static/vendor/sse.js"></script>
</head>
<body>
<div class="wrap" hx-ext="sse" sse-connect="/ui/tasks/{id}/stream">
  <header class="top">
    <h1>{title}</h1>
    <span class="sub"><a href="/">&larr; new task</a> &middot; project <code>{project}</code></span>
  </header>

  <!-- Each region listens for one named SSE event, so a single stream feeds
       four independent parts of the page. -->
  <div id="timeline" sse-swap="status" hx-swap="innerHTML">{timeline}</div>
  <div id="done" sse-swap="done" hx-swap="innerHTML">{done}</div>
  <div id="spec" sse-swap="spec" hx-swap="innerHTML">{spec}</div>

  <h2 class="section">Debate</h2>
  <div id="debate" sse-swap="debate" hx-swap="beforeend">{debate}</div>

  <h2 class="section">Implementation</h2>
  <div id="terminal" class="terminal" sse-swap="build" hx-swap="beforeend">{build}</div>
</div>
<script>
  // Keep the newest build output in view without stealing the whole page.
  document.body.addEventListener('htmx:sseMessage', () => {{
    const t = document.getElementById('terminal');
    if (t) t.scrollTop = t.scrollHeight;
  }});
</script>
</body>
</html>"##,
        title = esc(&task.title),
        project = esc(&task.project),
    )
}

// ---------------------------------------------------------------------------
// The HTML event stream
// ---------------------------------------------------------------------------

/// `GET /ui/tasks/{id}/stream` — the same events as `/api/.../events`, but
/// rendered as HTML fragments and tagged with the region they belong to.
pub async fn stream(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let events = BroadcastStream::new(state.manager.subscribe()).filter_map(move |received| {
        match received {
            Ok((event_id, event)) if event_id == id => {
                let (name, html) = event_html(id, &event)?;
                Some(Ok(Event::default().event(name).data(html)))
            }
            Ok(_) => None,
            Err(BroadcastStreamRecvError::Lagged(_)) => Some(Ok(Event::default()
                .event("debate")
                .data(r#"<div class="notice warn">Some output was dropped — reload to catch up.</div>"#))),
        }
    });

    Sse::new(events).keep_alive(KeepAlive::default())
}

#[derive(Debug, Deserialize)]
pub struct ApproveForm {
    pub approve: String,
    #[serde(default)]
    pub spec: Option<String>,
}

/// `POST /ui/tasks/{id}/approve` — Gate 2 from the browser.
pub async fn approve(
    State(state): State<AppState>,
    Path(id): Path<TaskId>,
    Form(form): Form<ApproveForm>,
) -> Response {
    let approve = form.approve == "true";

    // Only send an edited spec when approving, and only if it has content.
    let spec = match (&form.spec, approve) {
        (Some(text), true) if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    };

    if !state.manager.decide(id, Decision { approve, spec }) {
        return (StatusCode::NOT_FOUND, Html("unknown task")).into_response();
    }

    // The gate is answered, so replace the panel with a plain read-only view.
    let body = state
        .manager
        .get(id)
        .map(|task| spec_readonly(&task))
        .unwrap_or_default();
    Html(body).into_response()
}
