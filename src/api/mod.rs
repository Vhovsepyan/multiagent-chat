//! Clients for the two model APIs.
//!
//! Both clients accept the same `Message` type and convert it to their own wire
//! format, because the two APIs disagree on names: Anthropic calls the model's
//! turn "assistant", Google calls it "model". Keeping one shared type here
//! means the debate loop never has to care which model it is talking to.

// The debate loop starts using these in Phase 3.
#![allow(dead_code)]

pub mod claude;
pub mod gemini;

/// Who said a given message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One turn of a conversation, in a form both clients understand.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    /// `impl Into<String>` accepts both `&str` and `String`, so callers can
    /// pass a literal without writing `.to_string()` every time.
    pub fn user(content: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}
