//! Integration tests for the Phase 8 API.
//!
//! These drive the real router with `tower::ServiceExt::oneshot`, so the full
//! extractor / handler / serialisation path runs without binding a port — two
//! test runs can never collide, and nothing here reaches the network.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::config::Config;
use crate::project::{Project, ProjectSource};
use crate::task::{Decision, TaskEvent, TaskStatus};
use crate::web::{AppState, router};
use crate::workspace::LocalWorkspaceProvider;

/// A config pointing at a throwaway workspace, with fake keys.
///
/// No test in this file makes an API call: creating a task spawns the pipeline,
/// which fails on the first request and marks the task Failed. That is fine —
/// these tests are about the HTTP surface, not the debate.
fn test_state(tag: &str) -> (AppState, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("mac-web-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let config = Config {
        gemini_api_key: "test".into(),
        anthropic_api_key: "test".into(),
        workspace_root: Some(root.clone()),
        max_rounds: 1,
        gemini_model: "test-model".into(),
        critic_model: "test-model".into(),
        implementer_model: "test-model".into(),
        permission_mode: "acceptEdits".into(),
        port: 0,
    };
    let provider = LocalWorkspaceProvider::new(root.join("task-workspaces")).unwrap();
    (
        AppState::with_workspace(config, std::sync::Arc::new(provider)),
        root,
    )
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn health_reports_ok() {
    let (state, _root) = test_state("health");
    let response = router(state).oneshot(get("/api/health")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["status"], "ok");
}

#[tokio::test]
async fn projects_lists_registered_repositories_not_workspace_directories() {
    let (state, root) = test_state("projects");
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    let project = Project::new(
        "Beta",
        ProjectSource::github("openai/beta").unwrap(),
        "main",
    )
    .unwrap();
    state.projects.register(project).unwrap();

    let response = router(state).oneshot(get("/api/projects")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let projects = body_json(response).await["projects"].clone();
    assert_eq!(projects.as_array().unwrap().len(), 1);
    assert_eq!(projects[0]["name"], "Beta");
    assert_eq!(projects[0]["source"]["repository"], "openai/beta");

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn projects_can_be_registered_through_the_api() {
    let (state, root) = test_state("register");
    let response = router(state.clone())
        .oneshot(post(
            "/api/projects",
            json!({"name": "Engine", "repository": "https://github.com/openai/engine.git", "default_branch": "main"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    assert_eq!(body["source"]["provider"], "github");
    assert_eq!(body["source"]["repository"], "openai/engine");
    assert_eq!(state.projects.list().len(), 1);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn feature_and_bug_fix_validate_registered_projects() {
    for kind in ["feature", "bug_fix"] {
        let (state, root) = test_state(kind);
        let unknown = uuid::Uuid::new_v4();
        let response = router(state)
            .oneshot(post(
                "/api/tasks",
                json!({"kind": kind, "title": "Change", "description": "Do it", "project_id": unknown}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            body_json(response).await["error"]
                .as_str()
                .unwrap()
                .contains("registered")
        );
        std::fs::remove_dir_all(root).ok();
    }
}

#[tokio::test]
async fn creating_a_new_project_task_returns_typed_task_without_user_path() {
    let (state, root) = test_state("create");
    let app = router(state);

    let response = app
        .oneshot(post(
            "/api/tasks",
            json!({
                "kind": "new_project",
                "title": "Renamer",
                "description": "search and replace",
                "technology": "rust",
                "output": "reviewable_result"
            }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let task = body_json(response).await;
    assert_eq!(task["title"], "Renamer");
    assert_eq!(task["kind"], "new_project");
    assert_eq!(task["technology"], "rust");
    assert!(task["project_id"].is_null());
    assert!(task["id"].is_string());
    assert!(!root.join("renamer").exists());

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn creating_a_task_rejects_an_empty_title() {
    let (state, root) = test_state("empty-title");

    let response = router(state)
        .oneshot(post(
            "/api/tasks",
            json!({"kind": "new_project", "title": "   ", "description": "d", "technology": "rust", "output": "reviewable_result"}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("title")
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn registering_a_project_rejects_non_github_paths() {
    let (state, root) = test_state("source-validation");

    let response = router(state)
        .oneshot(post(
            "/api/projects",
            json!({"name": "escape", "repository": "C:/private/repo", "default_branch": "main"}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn fetching_an_unknown_task_is_404() {
    let (state, root) = test_state("missing");
    let id = uuid::Uuid::new_v4();

    let response = router(state)
        .oneshot(get(&format!("/api/tasks/{id}")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_malformed_task_id_is_rejected() {
    let (state, root) = test_state("badid");

    let response = router(state)
        .oneshot(get("/api/tasks/not-a-uuid"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn a_created_task_can_be_fetched_back() {
    let (state, root) = test_state("roundtrip");
    let task = state.manager.create("Renamer", "desc", "renamer");

    let response = router(state)
        .oneshot(get(&format!("/api/tasks/{}", task.id)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let fetched = body_json(response).await;
    assert_eq!(fetched["id"], task.id.to_string());
    assert_eq!(fetched["status"], "created");
    std::fs::remove_dir_all(&root).ok();
}

/// Approving something that is not at the gate must not silently succeed.
#[tokio::test]
async fn approving_a_task_that_is_not_waiting_is_409() {
    let (state, root) = test_state("early-approve");
    let task = state.manager.create("t", "d", "p");

    let response = router(state)
        .oneshot(post(
            &format!("/api/tasks/{}/approve", task.id),
            json!({"approve": true}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CONFLICT);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn approving_at_the_gate_records_the_decision() {
    let (state, root) = test_state("approve");
    let task = state.manager.create("t", "d", "p");
    state.manager.emitter(task.id).emit(TaskEvent::Spec {
        markdown: "generated spec".into(),
        path: "SPEC.md".into(),
    });
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::WaitingForApproval);

    let response = router(state.clone())
        .oneshot(post(
            &format!("/api/tasks/{}/approve", task.id),
            json!({"approve": true}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let decision = state.manager.decision(task.id).expect("decision recorded");
    assert!(decision.approve);
    assert_eq!(decision.spec.as_deref(), Some("generated spec"));
    assert_eq!(state.manager.get(task.id).unwrap().spec, decision.spec);
    std::fs::remove_dir_all(&root).ok();
}

/// DP-10: the edited document rides along with the approval.
#[tokio::test]
async fn an_edited_spec_is_carried_on_the_approval() {
    let (state, root) = test_state("edited");
    let task = state.manager.create("t", "d", "p");
    state.manager.emitter(task.id).emit(TaskEvent::Spec {
        markdown: "original".into(),
        path: "SPEC.md".into(),
    });
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::WaitingForApproval);

    let response = router(state.clone())
        .oneshot(post(
            &format!("/api/tasks/{}/approve", task.id),
            json!({"approve": true, "spec": "## Problem\nedited by hand"}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let decision = state.manager.decision(task.id).unwrap();
    assert_eq!(decision.spec.as_deref(), Some("## Problem\nedited by hand"));
    assert_eq!(state.manager.get(task.id).unwrap().spec, decision.spec);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn an_empty_edited_spec_is_rejected() {
    let (state, root) = test_state("empty-spec");
    let task = state.manager.create("t", "d", "p");
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::WaitingForApproval);

    let response = router(state.clone())
        .oneshot(post(
            &format!("/api/tasks/{}/approve", task.id),
            json!({"approve": true, "spec": "   "}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(state.manager.decision(task.id).is_none());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn rejecting_is_recorded_too() {
    let (state, root) = test_state("reject");
    let task = state.manager.create("t", "d", "p");
    state.manager.emitter(task.id).emit(TaskEvent::Spec {
        markdown: "generated spec".into(),
        path: "SPEC.md".into(),
    });
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::WaitingForApproval);

    let response = router(state.clone())
        .oneshot(post(
            &format!("/api/tasks/{}/approve", task.id),
            json!({"approve": false}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(!state.manager.decision(task.id).unwrap().approve);
    assert_eq!(
        state.manager.get(task.id).unwrap().spec.as_deref(),
        Some("generated spec")
    );
    std::fs::remove_dir_all(&root).ok();
}

/// DP-11's whole point: the parked pipeline must wake when the answer lands,
/// and it must also cope with an answer that arrives BEFORE it parks.
#[tokio::test]
async fn the_gate_wakes_a_waiting_pipeline() {
    let (state, root) = test_state("gate-wake");
    let task = state.manager.create("t", "d", "p");

    let manager = state.manager.clone();
    let id = task.id;
    let waiter = tokio::spawn(async move { manager.await_decision(id).await });

    // Give the waiter a chance to park before answering.
    tokio::task::yield_now().await;
    state.manager.decide(
        task.id,
        Decision {
            approve: true,
            spec: None,
        },
    );

    let decision = waiter.await.unwrap().expect("should wake");
    assert!(decision.approve);
    std::fs::remove_dir_all(&root).ok();
}

/// The missed-wakeup case: `notify_one` stores a permit, and the state is
/// checked before parking, so an early answer is never lost.
#[tokio::test]
async fn an_answer_before_the_gate_is_not_lost() {
    let (state, root) = test_state("gate-early");
    let task = state.manager.create("t", "d", "p");

    state.manager.decide(
        task.id,
        Decision {
            approve: false,
            spec: None,
        },
    );

    let decision = state
        .manager
        .await_decision(task.id)
        .await
        .expect("the early answer should still be seen");
    assert!(!decision.approve);
    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// SSE (Phase 9)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn events_for_an_unknown_task_is_404() {
    let (state, root) = test_state("sse-missing");
    let id = uuid::Uuid::new_v4();

    let response = router(state)
        .oneshot(get(&format!("/api/tasks/{id}/events")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn events_responds_as_an_sse_stream() {
    let (state, root) = test_state("sse-headers");
    let task = state.manager.create("t", "d", "p");

    let response = router(state)
        .oneshot(get(&format!("/api/tasks/{}/events", task.id)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        content_type.starts_with("text/event-stream"),
        "unexpected content-type: {content_type}"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The stream must carry real events, and only for the task asked for.
#[tokio::test]
async fn the_stream_carries_events_for_this_task_only() {
    use http_body_util::BodyExt;

    let (state, root) = test_state("sse-body");
    let watched = state.manager.create("watched", "d", "p");
    let other = state.manager.create("other", "d", "p");

    let response = router(state.clone())
        .oneshot(get(&format!("/api/tasks/{}/events", watched.id)))
        .await
        .unwrap();
    let mut body = response.into_body();

    // Noise on a different task must not appear in this stream.
    state.manager.emitter(other.id).notice("not for you");
    state.manager.emitter(watched.id).emit(TaskEvent::Proposal {
        round: 1,
        text: "use Rust".into(),
    });

    let frame = body.frame().await.unwrap().unwrap();
    let chunk = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();

    assert!(chunk.contains("\"type\":\"proposal\""), "got: {chunk}");
    assert!(chunk.contains("use Rust"), "got: {chunk}");
    assert!(
        !chunk.contains("not for you"),
        "leaked another task: {chunk}"
    );

    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// UI (Phase 10)
// ---------------------------------------------------------------------------

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn post_form(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn the_index_page_is_served_from_disk() {
    let (state, root) = test_state("ui-index");
    let response = router(state).oneshot(get("/")).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(
        html.contains("multiagent-chat"),
        "got: {}",
        &html[..80.min(html.len())]
    );
    assert!(
        html.contains("htmx.min.js"),
        "htmx should be vendored locally"
    );
    assert!(html.contains("GitHub repository"));
    assert!(!html.contains("WORKSPACE_ROOT"));
    assert!(!html.contains("new_project\" name=\"new_project"));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn the_stylesheet_is_served() {
    let (state, root) = test_state("ui-css");
    let response = router(state)
        .oneshot(get("/static/style.css"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn the_project_picker_lists_projects() {
    let (state, root) = test_state("ui-projects");
    let project = Project::new(
        "Alpha",
        ProjectSource::github("openai/alpha").unwrap(),
        "main",
    )
    .unwrap();
    let id = project.id;
    state.projects.register(project).unwrap();

    let response = router(state).oneshot(get("/ui/projects")).await.unwrap();
    let html = body_text(response).await;

    assert!(
        html.contains(&format!(r#"<option value="{id}">Alpha</option>"#)),
        "got: {html}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn the_task_page_renders_history_and_attaches_the_stream() {
    let (state, root) = test_state("ui-page");
    let task = state.manager.create("Renamer", "d", "p");
    let emitter = state.manager.emitter(task.id);
    emitter.emit(TaskEvent::Proposal {
        round: 1,
        text: "use Rust".into(),
    });

    let response = router(state)
        .oneshot(get(&format!("/task/{}", task.id)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    // History is rendered inline, which is what closes the subscribe race.
    assert!(html.contains("use Rust"), "history should be replayed");
    assert!(html.contains(&format!(r#"sse-connect="/ui/tasks/{}/stream""#, task.id)));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn an_unknown_task_page_is_404() {
    let (state, root) = test_state("ui-404");
    let response = router(state)
        .oneshot(get(&format!("/task/{}", uuid::Uuid::new_v4())))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    std::fs::remove_dir_all(&root).ok();
}

/// Everything the models write is rendered into HTML, so it must be escaped.
#[tokio::test]
async fn model_output_is_escaped_not_executed() {
    let (state, root) = test_state("ui-escape");
    let task = state.manager.create("t", "d", "p");
    state.manager.emitter(task.id).emit(TaskEvent::Proposal {
        round: 1,
        text: "<script>alert('xss')</script>".into(),
    });

    let response = router(state)
        .oneshot(get(&format!("/task/{}", task.id)))
        .await
        .unwrap();
    let html = body_text(response).await;

    assert!(
        !html.contains("<script>alert"),
        "raw script tag leaked into the page"
    );
    assert!(html.contains("&lt;script&gt;"), "should be escaped: {html}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn creating_from_the_form_redirects_to_the_task() {
    let (state, root) = test_state("ui-create");

    let response = router(state)
        .oneshot(post_form(
            "/ui/tasks",
            "kind=new_project&title=Renamer&description=Build+it&technology=rust&output=reviewable_result",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let redirect = response
        .headers()
        .get("HX-Redirect")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(redirect.starts_with("/task/"), "got: {redirect}");
    assert!(!root.join("renamer").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn the_form_reports_a_missing_project() {
    let (state, root) = test_state("ui-noproject");
    let response = router(state)
        .oneshot(post_form(
            "/ui/tasks",
            "kind=feature&title=T&description=Change+it",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(response).await.contains("registered project"));
    std::fs::remove_dir_all(&root).ok();
}

/// The edited textarea is what gets built (DP-10) when approving from the page.
#[tokio::test]
async fn approving_from_the_page_carries_the_edited_spec() {
    let (state, root) = test_state("ui-approve");
    let task = state.manager.create("t", "d", "p");
    state.manager.emitter(task.id).emit(TaskEvent::Spec {
        markdown: "original".into(),
        path: "SPEC.md".into(),
    });
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::WaitingForApproval);

    let response = router(state.clone())
        .oneshot(post_form(
            &format!("/ui/tasks/{}/approve", task.id),
            "approve=true&spec=%23%23%20Problem%0Aedited",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(body.contains("## Problem\nedited"));
    assert!(!body.contains("original"));
    let decision = state.manager.decision(task.id).unwrap();
    assert!(decision.approve);
    assert_eq!(decision.spec.as_deref(), Some("## Problem\nedited"));
    assert_eq!(state.manager.get(task.id).unwrap().spec, decision.spec);
    std::fs::remove_dir_all(&root).ok();
}

/// Rejecting must never smuggle the textarea through as an edited spec.
#[tokio::test]
async fn rejecting_from_the_page_discards_the_textarea() {
    let (state, root) = test_state("ui-reject");
    let task = state.manager.create("t", "d", "p");
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::WaitingForApproval);

    router(state.clone())
        .oneshot(post_form(
            &format!("/ui/tasks/{}/approve", task.id),
            "approve=false&spec=ignored",
        ))
        .await
        .unwrap();

    let decision = state.manager.decision(task.id).unwrap();
    assert!(!decision.approve);
    assert!(decision.spec.is_none(), "a rejection must not carry a spec");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn the_ui_stream_sends_named_html_events() {
    use http_body_util::BodyExt;

    let (state, root) = test_state("ui-stream");
    let task = state.manager.create("t", "d", "p");

    let response = router(state.clone())
        .oneshot(get(&format!("/ui/tasks/{}/stream", task.id)))
        .await
        .unwrap();
    let mut body = response.into_body();

    state.manager.emitter(task.id).emit(TaskEvent::Build {
        chunk: "compiling".into(),
    });

    let frame = body.frame().await.unwrap().unwrap();
    let chunk = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();

    // HTMX routes by the SSE event name, so it must be present.
    assert!(chunk.contains("event: build"), "got: {chunk}");
    assert!(chunk.contains("compiling"), "got: {chunk}");
    std::fs::remove_dir_all(&root).ok();
}
