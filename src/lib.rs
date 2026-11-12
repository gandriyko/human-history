//! human-history: an offline, humanized history browser for coding-agent sessions.
//! The `human-history` binary runs `cli::main`.

pub mod cli;
pub mod config;
pub mod db;
pub mod git;
pub mod herdr;
pub mod indexer;
pub mod mcp;
pub mod model;
pub mod parsers;
pub mod resume;
pub mod search;
#[cfg(test)]
mod testutil;
pub mod tui;
