//! The workspace agent abstraction, used by the Worker role.
//!
//! A coding agent is not a chat agent with a longer prompt: it runs as a
//! process inside a task workspace, edits files, and runs commands. It
//! therefore gets its own trait and request type, while the existing timeout,
//! process-cleanup and environment-filtering guarantees stay inside the
//! provider adapter.

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;

use crate::agent::CodingTool;
use crate::task::Emitter;

/// One implementation job.
///
/// `spec_path` is deliberately OUTSIDE `workspace`: the approved specification
/// is an orchestration artifact and must not show up in the resulting diff.
#[derive(Debug, Clone, Copy)]
pub struct CodingTaskRequest<'a> {
    /// Directory the agent runs in and may modify.
    pub workspace: &'a Path,
    /// Absolute path of the authoritative approved specification artifact.
    pub spec_path: &'a Path,
    /// Task-kind-specific instructions, prepared by `workflow.rs`.
    pub instructions: &'a str,
}

/// What ran. Kept minimal on purpose: milestone reporting belongs to a later
/// task, and fields nothing fills in would only be misleading.
#[derive(Debug, Clone)]
pub struct CodingTaskResult {
    pub tool: CodingTool,
    pub model: String,
}

/// An agent that implements a specification inside a workspace.
///
/// Output streaming stays on the `Emitter` (DP-9) rather than becoming a return
/// value, so a long build still reaches the browser line by line.
#[async_trait]
pub trait CodingAgent: Send + Sync {
    /// Which tool this agent runs.
    fn tool(&self) -> CodingTool;

    /// The model it implements with.
    fn model(&self) -> &str;

    /// Run to completion. `Ok` means the agent exited successfully, not that
    /// verification passed — that is the pipeline stage that follows.
    async fn execute(
        &self,
        request: CodingTaskRequest<'_>,
        emitter: &Emitter,
    ) -> Result<CodingTaskResult>;
}
