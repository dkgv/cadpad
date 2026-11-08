//! Authenticated local app connection and an MCP bridge; no project ownership.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

pub const WIRE_VERSION: u32 = 2;
pub const MAX_FRAME: usize = 12 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub protocol_version: u32,
    pub endpoint: PathBuf,
    pub token: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Hello {
        protocol_version: u32,
        token: String,
        client_name: String,
    },
    Call {
        id: u64,
        name: String,
        arguments: Value,
    },
    Artifact {
        id: u64,
        workspace: String,
        revision: String,
        handle: String,
    },
    ArtifactChunk {
        id: u64,
        workspace: String,
        revision: String,
        handle: String,
        range: cadpad_core::artifacts::Chunk,
    },
    Definitions {
        id: u64,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

#[cfg(unix)]
mod local;
pub mod protocol;
#[cfg(unix)]
pub use local::{AppServer, Client, SessionInfo};
#[cfg(unix)]
pub mod mcp;
