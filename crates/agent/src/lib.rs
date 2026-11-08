//! Bounded built-in agent. Providers own wire formats; the application core owns
//! all project mutations, geometry, revisions and actor cancellation. CLI backends
//! reuse Claude / Codex / Cursor subscription auth and call Cadpad through MCP.
pub mod cli;
pub mod credentials;
pub mod provider;
pub mod references;
pub mod runner;
pub mod service;
