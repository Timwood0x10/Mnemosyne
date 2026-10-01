//! Blocking-pool bridge used by the MCP tool handlers.
//!
//! The implementation moved to the neutral [`crate::blocking`] module once the
//! storage layer needed the same hop (audit batch G): a storage module
//! depending on `mcp` would have inverted the layering. This module stays as
//! the handler-facing path so every `crate::mcp::blocking::run` call site keeps
//! working unchanged.

pub use crate::blocking::run;
