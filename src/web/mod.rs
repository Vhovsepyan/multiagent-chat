//! Phase 8: the axum server behind the v2 web UI.
//!
//! `router()` is deliberately separate from `serve()` so integration tests can
//! drive the whole API in-process, with no port to bind and no chance of two
//! test runs colliding.

#![allow(dead_code)]

pub mod handlers;
pub mod pipeline;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

use crate::config::Config;
use crate::task::TaskManager;
use crate::ui;

/// What every handler gets a copy of.
///
/// Both fields are cheap to clone: `TaskManager` is an `Arc` inside, and the
/// config is wrapped in one here rather than copying its strings per request.
#[derive(Clone)]
pub struct AppState {
    pub manager: TaskManager,
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        AppState {
            manager: TaskManager::new(),
            config: Arc::new(config),
        }
    }
}

/// Build the API router. No I/O happens here, which is what makes it testable.
///
/// axum 0.8 spells path parameters `{id}`, not the `:id` of earlier versions.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/projects", get(handlers::list_projects))
        .route("/api/tasks", post(handlers::create_task))
        .route("/api/tasks/{id}", get(handlers::get_task))
        .route("/api/tasks/{id}/approve", post(handlers::approve_task))
        .route("/api/tasks/{id}/events", get(handlers::task_events))
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

    ui::header("multiagent-chat — web mode");
    ui::success(&format!("listening on http://{address}"));
    ui::system("the browser UI arrives in Phase 10; for now this serves the API");
    ui::system("stop with Ctrl-C");

    axum::serve(listener, app)
        .await
        .context("the web server stopped unexpectedly")?;
    Ok(())
}
