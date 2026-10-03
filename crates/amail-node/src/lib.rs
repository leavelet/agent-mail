//! The amail node: the per-host daemon that connects running agent sessions
//! (Claude Code, Codex) to an amail server, the `amail hook` client their
//! hooks run, and the launch logic behind `amail run`.

pub mod adapter;
pub mod codex_rpc;
pub mod hook_client;
pub mod inject;
pub mod mux;
pub mod node;
pub mod paths;
pub mod persist;
pub mod server;
pub mod team;
pub mod template;
pub mod wire;
pub mod worktree;

pub use node::{Config, Node, NodeHandle, request, start};
