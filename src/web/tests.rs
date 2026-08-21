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
use crate::task::{Decision, TaskStatus};
use crate::web::{AppState, router};

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
        workspace_root: root.clone(),
        max_rounds: 1,
        gemini_model: "test-model".into(),
        critic_model: "test-model".into(),
        implementer_model: "test-model".into(),
        permission_mode: "acceptEdits".into(),
        port: 0,
    };
    (AppState::new(config), root)
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
async fn projects_lists_workspace_directories() {
    let (state, root) = test_state("projects");
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    std::fs::create_dir_all(root.join("beta")).unwrap();
    // Hidden folders and loose files are not projects.
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("notes.txt"), "x").unwrap();

    let response = router(state).oneshot(get("/api/projects")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let projects = body_json(response).await["projects"].clone();
    assert_eq!(projects, json!(["alpha", "beta"]));

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn creating_a_task_returns_201_and_makes_the_project() {
    let (state, root) = test_state("create");
    let app = router(state);

    let response = app
        .oneshot(post(
            "/api/tasks",
            json!({"title": "Renamer", "description": "search and replace", "project": "renamer"}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let task = body_json(response).await;
    assert_eq!(task["title"], "Renamer");
    assert_eq!(task["project"], "renamer");
    assert!(task["id"].is_string());
    assert!(root.join("renamer").is_dir(), "project should be created");

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn creating_a_task_rejects_an_empty_title() {
    let (state, root) = test_state("empty-title");

    let response = router(state)
        .oneshot(post(
            "/api/tasks",
            json!({"title": "   ", "description": "d", "project": "p"}),
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

/// A project name must not be able to climb out of WORKSPACE_ROOT.
#[tokio::test]
async fn creating_a_task_rejects_a_path_traversal_project() {
    let (state, root) = test_state("traversal");

    let response = router(state)
        .oneshot(post(
            "/api/tasks",
            json!({"title": "t", "description": "d", "project": "../escape"}),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!root.parent().unwrap().join("escape").exists());
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
    assert!(decision.spec.is_none());
    std::fs::remove_dir_all(&root).ok();
}

/// DP-10: the edited document rides along with the approval.
#[tokio::test]
async fn an_edited_spec_is_carried_on_the_approval() {
    let (state, root) = test_state("edited");
    let task = state.manager.create("t", "d", "p");
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
