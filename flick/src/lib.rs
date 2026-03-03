pub mod agent;
pub mod config;
pub mod context;
pub mod credential;
pub mod error;
pub mod event;
pub mod model;
pub mod prompter;
pub mod provider;
pub mod sandbox;
pub mod tool;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKind {
    Messages,
    ChatCompletions,
}

impl std::fmt::Display for ApiKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Messages => f.write_str("messages"),
            Self::ChatCompletions => f.write_str("chat_completions"),
        }
    }
}
