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
pub(super) fn test_state(tag: &str) -> (AppState, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("mac-web-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let config = Config {
        execution: Default::default(),
        gemini_api_key: Some("test".into()),
        anthropic_api_key: Some("test".into()),
        workspace_root: Some(root.clone()),
        max_rounds: 1,
        gemini_model: "test-model".into(),
        critic_model: "test-critic-model".into(),
        implementer_model: "test-worker-model".into(),
        gemini_models: vec!["test-model-fast".into()],
        anthropic_models: Vec::new(),
        claude_code_models: Vec::new(),
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
async fn browser_origin_policy_rejects_foreign_requests_and_allows_local_ui() {
    let (state, root) = test_state("origin-policy");
    let app = router(state.clone());
    for method in ["GET", "POST", "OPTIONS"] {
        for origin in [
            "https://evil.example",
            "null",
            "http://localhost:1234",
            "http://127.0.0.1:0.evil.example",
        ] {
            let request = Request::builder()
                .method(method)
                .uri("/api/projects")
                .header("host", "127.0.0.1:0")
                .header("origin", origin)
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert!(
                !response
                    .headers()
                    .contains_key("access-control-allow-origin")
            );
        }
    }
    for headers in [
        [("sec-fetch-site", "cross-site")],
        [("host", "evil.example:0")],
    ] {
        let request = Request::builder()
            .uri("/api/projects")
            .header(headers[0].0, headers[0].1)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    for (index, host) in ["127.0.0.1:0", "localhost:0"].into_iter().enumerate() {
        let mut request = post(
            "/api/projects",
            json!({"name":host,"repository":format!("owner/repo-{index}")}),
        );
        request.headers_mut().insert("host", host.parse().unwrap());
        request
            .headers_mut()
            .insert("origin", format!("http://{host}").parse().unwrap());
        request
            .headers_mut()
            .insert("sec-fetch-site", "same-origin".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::CREATED
        );
    }
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn both_approval_endpoints_reject_early_terminal_and_duplicate_requests() {
    let (state, root) = test_state("approval-state-policy");
    let app = router(state.clone());
    for endpoint in ["api", "ui"] {
        for status in [
            TaskStatus::Created,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Rejected,
        ] {
            let task = state.manager.create("task", "description", "legacy");
            state.manager.emitter(task.id).emit(TaskEvent::Spec {
                markdown: "original".into(),
                path: "SPEC.md".into(),
            });
            state.manager.emitter(task.id).status(status);
            let uri = format!("/{endpoint}/tasks/{}/approve", task.id);
            let request = if endpoint == "api" {
                post(&uri, json!({"approve":true,"spec":"injected"}))
            } else {
                post_form(&uri, "approve=true&spec=injected")
            };
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::CONFLICT
            );
            let stored = state.manager.get(task.id).unwrap();
            assert_eq!(stored.spec.as_deref(), Some("original"));
            assert!(stored.decision.is_none());
        }
        let task = state.manager.create("task", "description", "legacy");
        state
            .manager
            .emitter(task.id)
            .status(TaskStatus::WaitingForApproval);
        let uri = format!("/{endpoint}/tasks/{}/approve", task.id);
        for expected in [StatusCode::OK, StatusCode::CONFLICT] {
            let request = if endpoint == "api" {
                post(&uri, json!({"approve":true,"spec":"approved"}))
            } else {
                post_form(&uri, "approve=true&spec=approved")
            };
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                expected
            );
        }
    }
    std::fs::remove_dir_all(root).ok();
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
    assert_eq!(fetched["history"][0]["sequence"], 1);
    assert_eq!(fetched["history"][0]["event"]["type"], "task_created");
    let timestamp = fetched["history"][0]["timestamp"].as_str().unwrap();
    assert!(timestamp.ends_with('Z'), "not a UTC timestamp: {timestamp}");
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
    state.manager.emitter(task.id).emit(TaskEvent::Spec {
        markdown: "generated".into(),
        path: "SPEC.md".into(),
    });
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::WaitingForApproval);

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
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::WaitingForApproval);

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
    assert!(chunk.contains("\"sequence\":"), "got: {chunk}");
    assert!(chunk.contains("\"timestamp\":"), "got: {chunk}");
    assert!(chunk.contains("\"event\":{"), "got: {chunk}");
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
    emitter.emit(TaskEvent::Critique {
        round: 1,
        text: "add tests".into(),
        verdict: Some("needs_work".into()),
        reason: Some("coverage".into()),
    });

    let response = router(state)
        .oneshot(get(&format!("/task/{}", task.id)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    // History is rendered inline, which is what closes the subscribe race.
    assert!(html.contains("use Rust"), "history should be replayed");
    assert!(html.find("use Rust").unwrap() < html.find("add tests").unwrap());
    assert!(html.contains("class=\"event-time\""));
    assert!(html.contains("data-sequence=\"2\""));
    assert!(html.contains("data-sequence=\"3\""));
    assert!(html.contains(&format!(r#"sse-connect="/ui/tasks/{}/stream""#, task.id)));
    assert!(html.contains("Export Evidence"));
    assert!(html.contains(&format!("/api/tasks/{}/evidence", task.id)));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn evidence_endpoint_downloads_the_five_file_archive() {
    let (state, root) = test_state("evidence-download");
    let task = state.manager.create("Evidence", "download", "legacy");

    let response = router(state)
        .oneshot(get(&format!("/api/tasks/{}/evidence", task.id)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/zip");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains(&task.id.to_string())
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    assert_eq!(archive.len(), 5);
    for name in [
        "agent-session.jsonl",
        "DEVELOPMENT_LOG.md",
        "DECISIONS.md",
        "AGENT_USAGE.md",
        "FINAL_REPORT.md",
    ] {
        assert!(archive.by_name(name).is_ok(), "archive is missing {name}");
    }
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn evidence_endpoint_rejects_unknown_and_malformed_ids() {
    let (state, root) = test_state("evidence-errors");
    let app = router(state);

    let response = app
        .clone()
        .oneshot(get(&format!(
            "/api/tasks/{}/evidence",
            uuid::Uuid::new_v4()
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .oneshot(get("/api/tasks/../../escape/evidence"))
        .await
        .unwrap();
    assert!(!response.status().is_success());
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
    assert!(chunk.contains("class=\"event-time\""), "got: {chunk}");
    assert!(chunk.contains("data-sequence=\"2\""), "got: {chunk}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn successful_finished_event_updates_all_live_terminal_fragments() {
    use http_body_util::BodyExt;

    let (state, root) = test_state("ui-finished-success");
    let task = state.manager.create("t", "d", "p");
    state.manager.emitter(task.id).emit(TaskEvent::Spec {
        markdown: "approved spec".into(),
        path: "SPEC.md".into(),
    });
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::Implementing);
    let response = router(state.clone())
        .oneshot(get(&format!("/ui/tasks/{}/stream", task.id)))
        .await
        .unwrap();
    let mut body = response.into_body();

    state.manager.emitter(task.id).emit(TaskEvent::Finished {
        status: TaskStatus::Completed,
        error: None,
    });

    let frame = body.frame().await.unwrap().unwrap();
    let output = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    assert!(output.contains("event: status"), "got: {output}");
    assert!(output.contains(">Completed<"), "got: {output}");
    assert!(output.contains(r#"id="done" hx-swap-oob="innerHTML""#));
    assert!(output.contains("implementation and verification finished"));
    assert!(output.contains(r#"id="spec" hx-swap-oob="innerHTML""#));
    assert!(!output.contains(r#"class="step active">Build"#));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn failed_finished_event_replaces_the_live_implementing_status() {
    use http_body_util::BodyExt;

    let (state, root) = test_state("ui-finished-failed");
    let task = state.manager.create("t", "d", "p");
    state
        .manager
        .emitter(task.id)
        .status(TaskStatus::Implementing);
    let response = router(state.clone())
        .oneshot(get(&format!("/ui/tasks/{}/stream", task.id)))
        .await
        .unwrap();
    let mut body = response.into_body();

    state.manager.emitter(task.id).emit(TaskEvent::Finished {
        status: TaskStatus::Failed,
        error: Some("verification failed".into()),
    });

    let frame = body.frame().await.unwrap().unwrap();
    let output = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    assert!(output.contains("event: status"), "got: {output}");
    assert!(output.contains(">Failed<"), "got: {output}");
    assert!(output.contains(r#"id="done" hx-swap-oob="innerHTML""#));
    assert!(output.contains("Failed: verification failed"));
    assert!(!output.contains(r#"class="step active">Build"#));
    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// Task 0005: per-task agent and model selection
// ---------------------------------------------------------------------------

/// A state whose credentials are unmistakable, so a leak into a response body
/// cannot hide behind a word that appears in normal output.
fn secret_state(tag: &str) -> (AppState, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("mac-web-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let config = Config {
        execution: Default::default(),
        gemini_api_key: Some("gemini-credential-must-not-leak".into()),
        anthropic_api_key: Some("anthropic-credential-must-not-leak".into()),
        workspace_root: Some(root.clone()),
        max_rounds: 1,
        gemini_model: "test-model".into(),
        critic_model: "test-critic-model".into(),
        implementer_model: "test-worker-model".into(),
        gemini_models: vec!["test-model-fast".into()],
        anthropic_models: Vec::new(),
        claude_code_models: Vec::new(),
        permission_mode: "acceptEdits".into(),
        port: 0,
    };
    let provider = LocalWorkspaceProvider::new(root.join("task-workspaces")).unwrap();
    (
        AppState::with_workspace(config, std::sync::Arc::new(provider)),
        root,
    )
}

/// Required test 10: the options endpoint is the browser's only view of agent
/// configuration, so it must carry names and nothing else.
#[tokio::test]
async fn agent_options_expose_models_but_never_credentials() {
    let (state, root) = secret_state("agent-options");

    let response = router(state).oneshot(get("/api/agents")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;

    assert!(!body.contains("credential"), "credentials leaked: {body}");
    assert!(!body.to_lowercase().contains("api_key"));
    assert!(!body.contains("permission_mode"));
    assert!(!body.contains("workspace_root"));

    let options: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(options["chat_providers"][0]["id"], "gemini");
    assert_eq!(options["chat_providers"][1]["id"], "anthropic");
    assert_eq!(options["coding_tools"][0]["id"], "claude_code");
    // The configured default is always offered, plus any extra models.
    assert_eq!(
        options["chat_providers"][0]["models"],
        json!(["test-model", "test-model-fast"])
    );
    assert_eq!(options["chat_providers"][0]["default_model"], "test-model");
    assert_eq!(options["defaults"]["proposer"]["provider"], "gemini");
    assert_eq!(options["defaults"]["critic"]["provider"], "anthropic");
    assert_eq!(options["defaults"]["worker"]["tool"], "claude_code");
    assert_eq!(options["defaults"]["worker"]["model"], "test-worker-model");

    std::fs::remove_dir_all(&root).ok();
}

/// Required tests 2-4: an explicit choice for each role is stored on the task.
#[tokio::test]
async fn creating_a_task_stores_the_selected_agents_per_role() {
    let (state, root) = test_state("agent-select");

    let response = router(state.clone())
        .oneshot(post(
            "/api/tasks",
            json!({
                "kind": "new_project",
                "title": "Renamer",
                "description": "search and replace",
                "technology": "rust",
                "output": "reviewable_result",
                "agents": {
                    "proposer": {"provider": "gemini", "model": "test-model-fast"},
                    "critic": {"provider": "gemini", "model": "test-model"},
                    "worker": {"tool": "claude_code", "model": "test-worker-model"}
                }
            }),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let task = body_json(response).await;
    assert_eq!(task["agents"]["proposer"]["provider"], "gemini");
    assert_eq!(task["agents"]["proposer"]["model"], "test-model-fast");
    // Both chat roles may run on one provider with different models.
    assert_eq!(task["agents"]["critic"]["provider"], "gemini");
    assert_eq!(task["agents"]["critic"]["model"], "test-model");
    assert_eq!(task["agents"]["worker"]["tool"], "claude_code");

    let id: crate::task::TaskId = task["id"].as_str().unwrap().parse().unwrap();
    let stored = state.manager.get(id).unwrap();
    assert_eq!(stored.agents.proposer.model, "test-model-fast");
    assert_eq!(stored.agents.critic.model, "test-model");

    std::fs::remove_dir_all(&root).ok();
}

/// Requirement 5: a request that says nothing about agents behaves exactly as
/// it did before this feature existed.
#[tokio::test]
async fn a_request_without_agents_uses_the_configured_defaults() {
    let (state, root) = test_state("agent-default");

    let response = router(state)
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
    assert_eq!(task["agents"]["proposer"]["provider"], "gemini");
    assert_eq!(task["agents"]["proposer"]["model"], "test-model");
    assert_eq!(task["agents"]["critic"]["provider"], "anthropic");
    assert_eq!(task["agents"]["critic"]["model"], "test-critic-model");
    assert_eq!(task["agents"]["worker"]["model"], "test-worker-model");

    std::fs::remove_dir_all(&root).ok();
}

/// Required tests 5-8: every invalid combination is refused, and none of them
/// is quietly replaced by something that would have worked.
#[tokio::test]
async fn invalid_agent_selections_are_rejected_rather_than_substituted() {
    let base = json!({
        "kind": "new_project",
        "title": "Renamer",
        "description": "search and replace",
        "technology": "rust",
        "output": "reviewable_result"
    });
    let cases = [
        // 5: a provider this build does not serve. The message names what was
        // sent and what is accepted, so the client can fix the request.
        (
            "unknown-provider",
            json!({"proposer": {"provider": "openai"}}),
            vec!["proposer", "openai", "gemini", "anthropic"],
        ),
        // 7: a worker tool this build does not serve (Codex is task 0015).
        (
            "unknown-tool",
            json!({"worker": {"tool": "codex"}}),
            vec!["worker", "codex", "claude_code"],
        ),
        // 6: a model configured for a different provider.
        (
            "wrong-provider-model",
            json!({"proposer": {"provider": "gemini", "model": "test-critic-model"}}),
            vec![
                "proposer model",
                "test-critic-model",
                "Gemini",
                "test-model",
            ],
        ),
        // 6: a model no provider offers.
        (
            "unknown-model",
            json!({"critic": {"provider": "anthropic", "model": "claude-imaginary"}}),
            vec!["critic model", "claude-imaginary", "Anthropic"],
        ),
        // 8: an explicitly empty model.
        (
            "empty-model",
            json!({"worker": {"model": ""}}),
            vec!["worker model cannot be empty"],
        ),
    ];

    for (tag, agents, expected) in cases {
        let (state, root) = test_state(&format!("agent-invalid-{tag}"));
        let mut body = base.clone();
        body["agents"] = agents;

        let response = router(state.clone())
            .oneshot(post("/api/tasks", body))
            .await
            .unwrap();

        // One shape for every invalid selection: 400 with {"error": "..."}.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{tag}");
        let body = body_json(response).await;
        let error = body["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{tag} should answer with an error field, got {body}"))
            .to_string();
        for fragment in expected {
            assert!(
                error.contains(fragment),
                "{tag} error should mention {fragment:?}: {error}"
            );
        }
        assert!(
            !error.contains("at line"),
            "{tag} should not leak a byte offset: {error}"
        );
        assert_eq!(state.manager.len(), 0, "{tag} must not create a task");
        std::fs::remove_dir_all(&root).ok();
    }
}

/// Required test 9: the selection is frozen on the task. Later configuration
/// changes — a different default model, a different offer — cannot rewrite what
/// a created run is using.
#[tokio::test]
async fn a_stored_selection_ignores_later_configuration_changes() {
    let (state, root) = test_state("agent-frozen");

    let response = router(state.clone())
        .oneshot(post(
            "/api/tasks",
            json!({
                "kind": "new_project",
                "title": "Renamer",
                "description": "search and replace",
                "technology": "rust",
                "output": "reviewable_result",
                "agents": {"proposer": {"provider": "gemini", "model": "test-model-fast"}}
            }),
        ))
        .await
        .unwrap();
    let id: crate::task::TaskId = body_json(response).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // The operator edits .env and restarts nothing: a new catalogue now offers
    // different models and a different default.
    let mut changed = crate::agent::test_config();
    changed.gemini_model = "gemini-brand-new".into();
    changed.critic_model = "claude-brand-new".into();
    let later = crate::agent::AgentCatalogue::from_config(&changed);
    assert_eq!(later.defaults().unwrap().proposer.model, "gemini-brand-new");

    let stored = state.manager.get(id).unwrap();
    assert_eq!(stored.agents.proposer.model, "test-model-fast");
    assert_eq!(stored.agents.critic.model, "test-critic-model");
    assert_eq!(stored.agents.worker.model, "test-worker-model");

    std::fs::remove_dir_all(&root).ok();
}

/// The form is the production path, and it carries the same six choices as the
/// JSON API (frontend cases 4 and 5).
#[tokio::test]
async fn the_form_submits_and_displays_the_selected_agents() {
    let (state, root) = test_state("ui-agent-select");

    let response = router(state.clone())
        .oneshot(post_form(
            "/ui/tasks",
            "kind=new_project&title=Renamer&description=Build+it&technology=rust&output=reviewable_result\
             &proposer_provider=anthropic&proposer_model=test-critic-model\
             &critic_provider=anthropic&critic_model=test-critic-model\
             &worker_tool=claude_code&worker_model=test-worker-model",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let task = state.manager.list().pop().unwrap();
    assert_eq!(
        task.agents.proposer.provider,
        crate::agent::ChatProvider::Anthropic
    );
    assert_eq!(task.agents.proposer.model, "test-critic-model");

    // Requirement 11: the task page reports what this run actually uses.
    let page = body_text(
        router(state)
            .oneshot(get(&format!("/task/{}", task.id)))
            .await
            .unwrap(),
    )
    .await;
    assert!(page.contains("Agents"), "no agents card: {page}");
    assert!(page.contains("Anthropic"));
    assert!(page.contains("test-critic-model"));
    assert!(page.contains("Claude Code"));

    std::fs::remove_dir_all(&root).ok();
}

/// An empty `<select>` is "not chosen", but a bad one is still refused.
#[tokio::test]
async fn the_form_defaults_blank_selectors_and_refuses_bad_ones() {
    let (state, root) = test_state("ui-agent-blank");
    let app = router(state.clone());

    let response = app
        .clone()
        .oneshot(post_form(
            "/ui/tasks",
            "kind=new_project&title=Renamer&description=Build+it&technology=rust&output=reviewable_result\
             &proposer_provider=&proposer_model=&critic_provider=&critic_model=&worker_tool=&worker_model=",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let task = state.manager.list().pop().unwrap();
    assert_eq!(task.agents.proposer.model, "test-model");

    let response = app
        .oneshot(post_form(
            "/ui/tasks",
            "kind=new_project&title=Renamer&description=Build+it&technology=rust&output=reviewable_result\
             &proposer_provider=openai",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(response).await.contains("not a supported"));
    assert_eq!(state.manager.len(), 1, "the bad request created a task");

    std::fs::remove_dir_all(&root).ok();
}

/// Frontend cases 1-3 as far as a server-side test can reach them: the form
/// ships the selectors, and it takes its options from the backend rather than
/// from hard-coded model names.
#[tokio::test]
async fn the_task_form_ships_agent_selectors_without_hard_coded_models() {
    let (state, root) = test_state("ui-agent-form");
    let html = body_text(router(state).oneshot(get("/")).await.unwrap()).await;

    for field in [
        "proposer_provider",
        "proposer_model",
        "critic_provider",
        "critic_model",
        "worker_tool",
        "worker_model",
    ] {
        assert!(html.contains(field), "form is missing {field}");
    }
    assert!(
        html.contains("/api/agents"),
        "options must come from the API"
    );
    assert!(
        !html.contains("claude-sonnet"),
        "model names must not be hard-coded"
    );
    assert!(
        !html.contains("gemini-3"),
        "model names must not be hard-coded"
    );

    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// Task 0005 follow-up: independent provider availability, uniform JSON errors
// ---------------------------------------------------------------------------

/// A state that configures only the named providers, so an installation with
/// one key — or none — can be exercised end to end.
fn state_with_credentials(
    tag: &str,
    gemini: bool,
    anthropic: bool,
) -> (AppState, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("mac-web-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let config = Config {
        execution: Default::default(),
        gemini_api_key: gemini.then(|| "gemini-credential-must-not-leak".into()),
        anthropic_api_key: anthropic.then(|| "anthropic-credential-must-not-leak".into()),
        workspace_root: Some(root.clone()),
        max_rounds: 1,
        gemini_model: "test-model".into(),
        critic_model: "test-critic-model".into(),
        implementer_model: "test-worker-model".into(),
        gemini_models: Vec::new(),
        anthropic_models: Vec::new(),
        claude_code_models: Vec::new(),
        permission_mode: "acceptEdits".into(),
        port: 0,
    };
    let provider = LocalWorkspaceProvider::new(root.join("task-workspaces")).unwrap();
    (
        AppState::with_workspace(config, std::sync::Arc::new(provider)),
        root,
    )
}

fn new_project_body(agents: Option<Value>) -> Value {
    let mut body = json!({
        "kind": "new_project",
        "title": "Renamer",
        "description": "search and replace",
        "technology": "rust",
        "output": "reviewable_result"
    });
    if let Some(agents) = agents {
        body["agents"] = agents;
    }
    body
}

/// A provider with no credential is not offered, and the one that is configured
/// is unaffected. The worker keeps its own authentication, so it stays offered.
#[tokio::test]
async fn agent_options_list_only_available_providers() {
    let (state, root) = state_with_credentials("agents-gemini-only", true, false);

    let response = router(state).oneshot(get("/api/agents")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_text(response).await;
    assert!(!body.contains("credential"), "credentials leaked: {body}");

    let options: Value = serde_json::from_str(&body).unwrap();
    let providers = options["chat_providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "got {providers:?}");
    assert_eq!(providers[0]["id"], "gemini");
    assert_eq!(options["coding_tools"][0]["id"], "claude_code");
    // The critic default is unavailable, so the UI is told what to configure
    // rather than being handed a substitute provider.
    assert!(options["defaults"].is_null());
    let unavailable = options["unavailable"].as_str().unwrap();
    assert!(unavailable.contains("critic"), "got: {unavailable}");
    assert!(
        unavailable.contains("ANTHROPIC_API_KEY"),
        "got: {unavailable}"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// Both keys configured is the pre-existing setup, and it must look exactly as
/// it did: both providers offered, defaults present and unchanged.
#[tokio::test]
async fn both_credentials_keep_the_previous_defaults() {
    let (state, root) = state_with_credentials("agents-both", true, true);

    let options = body_json(router(state).oneshot(get("/api/agents")).await.unwrap()).await;

    assert_eq!(options["chat_providers"].as_array().unwrap().len(), 2);
    assert_eq!(options["defaults"]["proposer"]["provider"], "gemini");
    assert_eq!(options["defaults"]["proposer"]["model"], "test-model");
    assert_eq!(options["defaults"]["critic"]["provider"], "anthropic");
    assert_eq!(options["defaults"]["worker"]["tool"], "claude_code");
    assert!(options.get("unavailable").is_none());

    std::fs::remove_dir_all(&root).ok();
}

/// With one provider configured the application still runs: a task that names
/// that provider for both chat roles is accepted, and one that falls back to an
/// unavailable default is refused with configuration guidance.
#[tokio::test]
async fn one_configured_provider_is_enough_to_create_a_task() {
    let (state, root) = state_with_credentials("agents-anthropic-only", false, true);
    let app = router(state.clone());

    let response = app
        .clone()
        .oneshot(post(
            "/api/tasks",
            new_project_body(Some(json!({
                "proposer": {"provider": "anthropic"},
                "critic": {"provider": "anthropic"}
            }))),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let task = body_json(response).await;
    assert_eq!(task["agents"]["proposer"]["provider"], "anthropic");
    assert_eq!(task["agents"]["proposer"]["model"], "test-critic-model");
    assert_eq!(task["agents"]["worker"]["model"], "test-worker-model");

    // The default proposer is Gemini, which this installation cannot serve.
    let response = app
        .oneshot(post("/api/tasks", new_project_body(None)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = body_json(response).await["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("proposer"), "got: {error}");
    assert!(error.contains("GEMINI_API_KEY"), "got: {error}");
    assert!(
        !error.contains("must-not-leak"),
        "credential leaked: {error}"
    );
    assert_eq!(
        state.manager.len(),
        1,
        "the refused task was created anyway"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// With no chat credential at all the server still starts and serves the UI;
/// only the chat roles are unavailable, and the worker tool remains offered.
#[tokio::test]
async fn no_chat_credentials_still_serves_the_application() {
    let (state, root) = state_with_credentials("agents-none", false, false);
    let app = router(state.clone());

    assert_eq!(
        app.clone()
            .oneshot(get("/api/health"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let options = body_json(app.clone().oneshot(get("/api/agents")).await.unwrap()).await;
    assert!(options["chat_providers"].as_array().unwrap().is_empty());
    assert_eq!(options["coding_tools"].as_array().unwrap().len(), 1);
    assert!(options["defaults"].is_null());

    let response = app
        .oneshot(post("/api/tasks", new_project_body(None)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("GEMINI_API_KEY")
    );

    std::fs::remove_dir_all(&root).ok();
}

/// Every malformed JSON body answers in the API's own shape — 400 with an
/// `error` string — rather than a framework-generated 422.
#[tokio::test]
async fn malformed_json_bodies_use_the_standard_error_response() {
    let (state, root) = test_state("json-errors");
    let app = router(state);

    let cases: [(&str, Request<Body>, Vec<&str>); 4] = [
        (
            "unknown-provider",
            post(
                "/api/tasks",
                new_project_body(Some(json!({"proposer": {"provider": "openai"}}))),
            ),
            vec!["invalid request body", "provider", "openai"],
        ),
        (
            "wrong-type",
            post("/api/tasks", json!({"kind": "new_project", "title": 7})),
            vec!["invalid request body", "title"],
        ),
        (
            "broken-syntax",
            Request::builder()
                .method("POST")
                .uri("/api/tasks")
                .header("content-type", "application/json")
                .body(Body::from("{not json"))
                .unwrap(),
            vec!["invalid request body"],
        ),
        (
            "missing-content-type",
            Request::builder()
                .method("POST")
                .uri("/api/projects")
                .body(Body::from("{}"))
                .unwrap(),
            vec!["invalid request body", "application/json"],
        ),
    ];

    for (tag, request, expected) in cases {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{tag}");
        let body = body_json(response).await;
        let error = body["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{tag} should answer with an error field, got {body}"))
            .to_string();
        for fragment in expected {
            assert!(
                error.contains(fragment),
                "{tag} error should mention {fragment:?}: {error}"
            );
        }
        assert!(
            !error.contains("at line"),
            "{tag} leaked an offset: {error}"
        );
    }

    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// Task 0008 follow-up: milestone badges update live
// ---------------------------------------------------------------------------

fn milestone_plan() -> Vec<crate::milestone::Milestone> {
    ["Project bootstrap", "Persistence layer"]
        .into_iter()
        .enumerate()
        .map(|(index, title)| crate::milestone::Milestone {
            id: format!("m{}", index + 1),
            order: u32::try_from(index + 1).unwrap(),
            title: title.into(),
            objective: format!("{title} objective"),
            verification_instructions: vec!["cargo test".into()],
            status: crate::milestone::MilestoneStatus::Pending,
            started_at: None,
            completed_at: None,
            worker_result_summary: None,
            commit: None,
        })
        .collect()
}

/// Every milestone lifecycle event must carry the refreshed badge list built
/// from current task state, not just another line of text.
#[tokio::test]
async fn milestone_events_refresh_the_status_badges_live() {
    use http_body_util::BodyExt;

    let (state, root) = test_state("ui-milestone-live");
    let task = state.manager.create("t", "d", "p");
    let emitter = state.manager.emitter(task.id);

    let response = router(state.clone())
        .oneshot(get(&format!("/ui/tasks/{}/stream", task.id)))
        .await
        .unwrap();
    let mut body = response.into_body();

    let plan = milestone_plan();
    emitter.emit(TaskEvent::MilestonePlanCreated {
        milestones: plan.clone(),
    });
    let created = String::from_utf8(
        body.frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    // The textual notice stays in the run log; the badge region is swapped
    // out-of-band on the same message, so SSE ordering is unchanged.
    assert!(created.contains("event: build"), "got: {created}");
    assert!(created.contains("Milestone plan created"), "got: {created}");
    assert!(
        created.contains(r#"<div id="milestones" hx-swap-oob="innerHTML">"#),
        "plan did not refresh the badge region: {created}"
    );
    assert_eq!(created.matches("milestone-status pending").count(), 2);

    emitter.emit(TaskEvent::MilestoneStarted {
        id: "m1".into(),
        order: 1,
        title: "Project bootstrap".into(),
        worker_tool: crate::agent::CodingTool::ClaudeCode,
        worker_model: "test-worker-model".into(),
    });
    let started = String::from_utf8(
        body.frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        started.contains(r#"<span class="milestone-status active">running</span>"#),
        "running badge missing: {started}"
    );
    assert!(started.contains("milestone-status pending"), "{started}");

    emitter.emit(TaskEvent::MilestoneCompleted {
        id: "m1".into(),
        order: 1,
        title: "Project bootstrap".into(),
        verification: Vec::new(),
        worker_result_summary: "done".into(),
    });
    let completed = String::from_utf8(
        body.frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        completed.contains(r#"<span class="milestone-status ok">passed</span>"#),
        "passed badge missing: {completed}"
    );

    emitter.emit(TaskEvent::MilestoneFailed {
        id: "m2".into(),
        order: 2,
        title: "Persistence layer".into(),
        verification: Vec::new(),
        worker_result_summary: None,
        error: "verification failed".into(),
    });
    let failed = String::from_utf8(
        body.frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(
        failed.contains(r#"<span class="milestone-status err">failed</span>"#),
        "failed badge missing: {failed}"
    );
    assert!(failed.contains("verification failed"), "{failed}");

    // A reload shows the same state the live badges already showed.
    let page = body_text(
        router(state)
            .oneshot(get(&format!("/task/{}", task.id)))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        page.contains(r#"<div id="milestones" hx-swap="innerHTML">"#),
        "{page}"
    );
    assert!(page.contains(r#"<span class="milestone-status ok">passed</span>"#));
    assert!(page.contains(r#"<span class="milestone-status err">failed</span>"#));
    assert!(
        page.contains("Milestone plan created"),
        "history is missing"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// A cancelled milestone must reach the badges too.
#[tokio::test]
async fn a_cancelled_milestone_updates_its_badge() {
    use http_body_util::BodyExt;

    let (state, root) = test_state("ui-milestone-cancel");
    let task = state.manager.create("t", "d", "p");
    let emitter = state.manager.emitter(task.id);
    emitter.emit(TaskEvent::MilestonePlanCreated {
        milestones: milestone_plan(),
    });

    let response = router(state.clone())
        .oneshot(get(&format!("/ui/tasks/{}/stream", task.id)))
        .await
        .unwrap();
    let mut body = response.into_body();

    emitter.emit(TaskEvent::MilestoneCancelled {
        id: "m1".into(),
        order: 1,
        title: "Project bootstrap".into(),
        reason: "task cancelled before milestone start".into(),
    });

    let cancelled = String::from_utf8(
        body.frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
            .to_vec(),
    )
    .unwrap();

    assert!(
        cancelled.contains(r#"<span class="milestone-status err">cancelled</span>"#),
        "cancelled badge missing: {cancelled}"
    );
    assert!(cancelled.contains("Milestone 1 cancelled"), "{cancelled}");

    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// Task 0007 follow-up: the export event follows a generated archive
// ---------------------------------------------------------------------------

/// Downloading records exactly one `EvidenceExported`, and only once the
/// archive exists. A task nobody exported has none.
#[tokio::test]
async fn evidence_export_records_one_audit_event_after_the_archive() {
    let (state, root) = test_state("evidence-audit");
    let task = state.manager.create("Evidence", "audit", "legacy");
    let app = router(state.clone());

    let exported = |state: &AppState| {
        state
            .manager
            .get(task.id)
            .unwrap()
            .history
            .iter()
            .filter(|recorded| matches!(recorded.event, TaskEvent::EvidenceExported { .. }))
            .count()
    };

    assert_eq!(exported(&state), 0, "no export happened yet");

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(get(&format!("/api/tasks/{}/evidence", task.id)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        // Recording happens after generation, so a downloaded archive always
        // exists when the event is present.
        assert!(!bytes.is_empty());
        assert_eq!(exported(&state), 1);
    }

    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// Task 0009: per-run Git mode and milestone commits
// ---------------------------------------------------------------------------

fn commit_event() -> TaskEvent {
    TaskEvent::MilestoneCommitCreated {
        id: "m1".into(),
        order: 1,
        title: "Project bootstrap".into(),
        commit: crate::git::MilestoneCommit {
            sha: "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678".into(),
            short_sha: "a1b2c3d".into(),
            message: "feat(milestone-01): Project bootstrap".into(),
        },
    }
}

/// The mode is part of the request, frozen on the task like the agent
/// selection, and defaults to the previous behavior.
#[tokio::test]
async fn task_creation_stores_the_requested_git_mode() {
    let (state, root) = test_state("git-mode-api");
    let app = router(state.clone());

    let response = app
        .clone()
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
    assert_eq!(body_json(response).await["git_mode"], "none");

    let response = app
        .clone()
        .oneshot(post(
            "/api/tasks",
            json!({
                "kind": "new_project",
                "title": "Renamer",
                "description": "search and replace",
                "technology": "rust",
                "output": "reviewable_result",
                "git_mode": "commit_per_milestone"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let task = body_json(response).await;
    assert_eq!(task["git_mode"], "commit_per_milestone");
    let id: crate::task::TaskId = task["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        state.manager.get(id).unwrap().git_mode,
        crate::git::GitMode::CommitPerMilestone
    );

    // An unsupported mode is refused in the standard shape, and creates nothing.
    let before = state.manager.len();
    let response = app
        .oneshot(post(
            "/api/tasks",
            json!({
                "kind": "new_project",
                "title": "Renamer",
                "description": "search and replace",
                "technology": "rust",
                "output": "reviewable_result",
                "git_mode": "push_to_github"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error = body_json(response).await["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(error.contains("git_mode"), "got: {error}");
    assert_eq!(state.manager.len(), before);

    std::fs::remove_dir_all(&root).ok();
}

/// The form carries the same choice, and an unknown value is refused.
#[tokio::test]
async fn the_form_submits_the_git_mode() {
    let (state, root) = test_state("git-mode-form");
    let app = router(state.clone());

    let response = app
        .clone()
        .oneshot(post_form(
            "/ui/tasks",
            "kind=new_project&title=Renamer&description=Build+it&technology=rust&output=reviewable_result\
             &git_mode=commit_per_milestone",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        state.manager.list().pop().unwrap().git_mode,
        crate::git::GitMode::CommitPerMilestone
    );

    // A blank select means the safe default.
    let response = app
        .clone()
        .oneshot(post_form(
            "/ui/tasks",
            "kind=new_project&title=Renamer&description=Build+it&technology=rust&output=reviewable_result&git_mode=",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        state
            .manager
            .list()
            .iter()
            .any(|task| task.git_mode == crate::git::GitMode::None)
    );

    let response = app
        .oneshot(post_form(
            "/ui/tasks",
            "kind=new_project&title=Renamer&description=Build+it&technology=rust&output=reviewable_result&git_mode=push",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(response)
            .await
            .contains("not a supported Git mode")
    );

    std::fs::remove_dir_all(&root).ok();
}

/// Required test 4/UI: the commit is stored on the milestone and shown in task
/// details, live and after a reload.
#[tokio::test]
async fn milestone_commits_are_stored_and_displayed() {
    use http_body_util::BodyExt;

    let (state, root) = test_state("git-commit-ui");
    let task = state.manager.create("t", "d", "p");
    let emitter = state.manager.emitter(task.id);
    emitter.emit(TaskEvent::MilestonePlanCreated {
        milestones: milestone_plan(),
    });

    let response = router(state.clone())
        .oneshot(get(&format!("/ui/tasks/{}/stream", task.id)))
        .await
        .unwrap();
    let mut body = response.into_body();

    emitter.emit(commit_event());

    let live = String::from_utf8(
        body.frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(live.contains("Milestone 1 committed"), "got: {live}");
    assert!(live.contains("a1b2c3d"), "short sha missing: {live}");
    // The badge region is refreshed with the commit line too.
    assert!(
        live.contains(r#"<div id="milestones" hx-swap-oob="innerHTML">"#),
        "got: {live}"
    );

    let stored = state.manager.get(task.id).unwrap();
    let commit = stored.milestones[0].commit.as_ref().unwrap();
    assert_eq!(commit.short_sha, "a1b2c3d");
    assert_eq!(commit.message, "feat(milestone-01): Project bootstrap");
    assert!(stored.milestones[1].commit.is_none());

    let page = body_text(
        router(state)
            .oneshot(get(&format!("/task/{}", task.id)))
            .await
            .unwrap(),
    )
    .await;
    assert!(page.contains("Commit: <code>a1b2c3d</code>"), "{page}");
    assert!(
        page.contains("No commits"),
        "the run's Git mode should be shown"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// Required test 8: commit creation reaches the evidence archive with safe
/// metadata only.
#[tokio::test]
async fn milestone_commits_appear_in_exported_evidence() {
    let (state, root) = test_state("git-commit-evidence");
    let task = state.manager.create("t", "d", "p");
    let emitter = state.manager.emitter(task.id);
    emitter.emit(TaskEvent::MilestonePlanCreated {
        milestones: milestone_plan(),
    });
    emitter.emit(commit_event());

    let response = router(state)
        .oneshot(get(&format!("/api/tasks/{}/evidence", task.id)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();

    let mut log = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("DEVELOPMENT_LOG.md").unwrap(),
        &mut log,
    )
    .unwrap();
    assert!(log.contains("Milestone 1 committed"), "{log}");
    assert!(log.contains("a1b2c3d"), "{log}");
    assert!(log.contains("feat(milestone-01)"), "{log}");

    let mut jsonl = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("agent-session.jsonl").unwrap(),
        &mut jsonl,
    )
    .unwrap();
    let recorded = jsonl
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|line| line["event"]["type"] == "milestone_commit_created")
        .expect("the commit event should be in the JSONL");
    assert_eq!(recorded["event"]["commit"]["short_sha"], "a1b2c3d");
    // Safe metadata only: no workspace path, no remote, no credential.
    assert!(!jsonl.contains("task-workspaces"), "workspace path leaked");
    assert!(!jsonl.to_lowercase().contains("api_key"));

    std::fs::remove_dir_all(&root).ok();
}
