//! Per-process agent-log substrate (AG-3).
//!
//! Moved here verbatim from `lattice-ai` so both agent transports (the ACP
//! client in `lattice-ai` and, eventually, the MCP adapter in
//! `lattice_ai::mcp`) can log through the same `AiLogger` / `AiLogMode`
//! machinery. `lattice-ai` re-exports every type below so its public API is
//! unchanged; nothing here is renamed -- the bus event stays
//! `"ai.log-pushed"`, the buffers stay `*ai:<provider>:<index>*`.

pub mod ai_log;
pub mod buffer_names;
pub mod modes;
