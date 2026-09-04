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

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

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
}

impl AppState {
    pub fn new(config: Config) -> Self {
        AppState {
            manager: TaskManager::new(),
            projects: ProjectStore::default(),
            workspaces: Arc::new(
                LocalWorkspaceProvider::temporary()
                    .expect("temporary workspace root should be available"),
            ),
            config: Arc::new(config),
        }
    }

    #[cfg(test)]
    pub fn with_workspace(config: Config, workspaces: Arc<dyn WorkspaceProvider>) -> Self {
        AppState {
            manager: TaskManager::new(),
            projects: ProjectStore::default(),
            workspaces,
            config: Arc::new(config),
        }
    }
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
        .route("/api/tasks", post(handlers::create_task))
        .route("/api/tasks/{id}", get(handlers::get_task))
        .route("/api/tasks/{id}/approve", post(handlers::approve_task))
        .route("/api/tasks/{id}/events", get(handlers::task_events))
        // --- the browser UI (DP-14: HTMX swaps HTML, so these render HTML) ---
        .route("/task/{id}", get(ui::task_page))
        .route("/ui/projects", get(ui::projects))
        .route("/ui/projects", post(ui::register_project))
        .route("/ui/tasks", post(ui::create))
        .route("/ui/tasks/{id}/stream", get(ui::stream))
        .route("/ui/tasks/{id}/approve", post(ui::approve))
        // DP-13: assets come off disk, so editing style.css needs only a
        // browser refresh. The path is relative to the working directory.
        .nest_service("/static", ServeDir::new(static_dir()))
        .fallback_service(ServeDir::new(static_dir()))
        // The frontend is served from this same origin in Phase 10, so CORS is
        // only here to keep a separately-served dev page working.
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Bind the port and serve until interrupted.
pub async fn serve(config: Config) -> Result<()> {
    let port = config.port;
    let state = AppState::new(config);
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
