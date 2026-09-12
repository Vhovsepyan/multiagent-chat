//! Phase 8: the axum server behind the v2 web UI.
//!
//! `router()` is deliberately separate from `serve()` so integration tests can
//! drive the whole API in-process, with no port to bind and no chance of two
//! test runs colliding.

#![allow(dead_code)]

pub mod handlers;
pub mod pipeline;
pub mod ui;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

use crate::agent::AgentCatalogue;
use crate::config::Config;
use crate::project::ProjectStore;
use crate::task::TaskManager;
use crate::workspace::{LocalWorkspaceProvider, WorkspaceProvider};

/// What every handler gets a copy of.
///
/// Both fields are cheap to clone: `TaskManager` is an `Arc` inside, and the
/// config is wrapped in one here rather than copying its strings per request.
#[derive(Clone)]
pub struct AppState {
    pub manager: TaskManager,
    pub projects: ProjectStore,
    pub workspaces: Arc<dyn WorkspaceProvider>,
    pub config: Arc<Config>,
    /// Which providers/models this installation offers (task 0005). Built once
    /// from `config`, so a task resolved against it never depends on a later
    /// environment change.
    pub catalogue: Arc<AgentCatalogue>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self::try_new(config).expect("durable task storage should be available")
    }

    pub fn try_new(config: Config) -> Result<Self> {
        let manager = task_manager(&config)?;
        Ok(AppState {
            manager,
            projects: ProjectStore::default(),
            workspaces: Arc::new(
                LocalWorkspaceProvider::temporary_with_limits(config.execution.clone())
                    .expect("temporary workspace root should be available"),
            ),
            catalogue: Arc::new(AgentCatalogue::from_config(&config)),
            config: Arc::new(config),
        })
    }

    #[cfg(test)]
    pub fn with_workspace(config: Config, workspaces: Arc<dyn WorkspaceProvider>) -> Self {
        let manager = task_manager(&config).expect("durable task storage should be available");
        AppState {
            manager,
            projects: ProjectStore::default(),
            workspaces,
            catalogue: Arc::new(AgentCatalogue::from_config(&config)),
            config: Arc::new(config),
        }
    }
}

fn task_manager(config: &Config) -> Result<TaskManager> {
    let runtime_root = config
        .workspace_root
        .clone()
        .unwrap_or_else(|| PathBuf::from(".runtime"));
    TaskManager::with_durable_history_limits_and_secrets(
        config.execution.history,
        [
            config.gemini_api_key.clone(),
            config.anthropic_api_key.clone(),
        ]
        .into_iter()
        .flatten(),
        runtime_root,
    )
}

/// Where the frontend assets live, relative to the working directory (DP-13).
///
/// Overridable with STATIC_DIR so the binary can be run from somewhere other
/// than the repo root.
fn static_dir() -> String {
    std::env::var("STATIC_DIR").unwrap_or_else(|_| "src/web/static".to_string())
}

/// Build the API router. No I/O happens here, which is what makes it testable.
///
/// axum 0.8 spells path parameters `{id}`, not the `:id` of earlier versions.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/projects", get(handlers::list_projects))
        .route("/api/projects", post(handlers::register_project))
        .route("/api/agents", get(handlers::agent_options))
        .route("/api/tasks", post(handlers::create_task))
        .route("/api/tasks/{id}", get(handlers::get_task))
        .route("/api/tasks/{id}/approve", post(handlers::approve_task))
        .route(
            "/api/tasks/{id}/publish",
            get(handlers::github_publish_preview).post(handlers::github_publish),
        )
        .route(
            "/api/tasks/{id}/publish/prepare",
            post(handlers::github_publish_prepare),
        )
        .route("/api/tasks/{id}/events", get(handlers::task_events))
        .route("/api/tasks/{id}/evidence", get(handlers::export_evidence))
        // --- the browser UI (DP-14: HTMX swaps HTML, so these render HTML) ---
        .route("/task/{id}", get(ui::task_page))
        .route("/ui/projects", get(ui::projects))
        .route("/ui/projects", post(ui::register_project))
        .route("/ui/tasks", post(ui::create))
        .route("/ui/tasks/{id}/stream", get(ui::stream))
        .route("/ui/tasks/{id}/approve", post(ui::approve))
        .route("/ui/tasks/{id}/publish", post(ui::publish))
        .route("/ui/tasks/{id}/publish/prepare", post(ui::prepare_publish))
        // DP-13: assets come off disk, so editing style.css needs only a
        // browser refresh. The path is relative to the working directory.
        .nest_service("/static", ServeDir::new(static_dir()))
        .fallback_service(ServeDir::new(static_dir()))
        .layer(axum::middleware::from_fn_with_state(
            state.config.port,
            require_local_origin,
        ))
        .with_state(state)
}

/// The unauthenticated local UI accepts only its configured loopback origins.
/// Origin checks include reads; Fetch Metadata also blocks cross-site forms
/// and navigations that omit Origin. Native clients may omit both headers.
async fn require_local_origin(
    axum::extract::State(port): axum::extract::State<u16>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;

    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let origins = hosts
        .iter()
        .map(|host| format!("http://{host}"))
        .collect::<Vec<_>>();
    let headers = request.headers();
    let bad_host = headers.get(header::HOST).is_some_and(|host| {
        host.to_str()
            .map_or(true, |host| !hosts.iter().any(|allowed| allowed == host))
    });
    let bad_origin = headers.get(header::ORIGIN).is_some_and(|origin| {
        origin.to_str().map_or(true, |origin| {
            !origins.iter().any(|allowed| allowed == origin)
        })
    });
    let cross_site = headers
        .get("sec-fetch-site")
        .is_some_and(|site| site != "same-origin" && site != "none");
    if bad_host || bad_origin || cross_site {
        return (StatusCode::FORBIDDEN, "request origin is not allowed").into_response();
    }
    next.run(request).await
}

/// Bind the port and serve until interrupted.
pub async fn serve(config: Config) -> Result<()> {
    let port = config.port;
    let state = AppState::try_new(config)?;
    let app = router(state);

    let address = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&address)
        .await
        .with_context(|| format!("could not bind {address} — is something already using it?"))?;

    crate::ui::header("multiagent-chat — web mode");
    crate::ui::success(&format!("open http://{address}"));
    crate::ui::system(&format!("serving assets from {}", static_dir()));
    crate::ui::system("stop with Ctrl-C");

    axum::serve(listener, app)
        .await
        .context("the web server stopped unexpectedly")?;
    Ok(())
}
