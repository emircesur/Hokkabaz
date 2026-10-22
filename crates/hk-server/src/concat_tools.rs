//! `concat_*` agent tools (feature `concat`): the Concat video editor bridge from `hk-concat`.
//!
//! The tools live in hk-concat (they only need hk-tools); this module is the registration point
//! next to [`crate::flow_tools`]: build a [`ConcatSession`] from the `[concat]` config and hand
//! `concat_tools(session)` to `AgentRuntime::add_tools`. The editor process starts lazily on the
//! first tool call, so registering costs nothing when Concat is not installed.
// port: none (Hokkabaz-native; see docs/CONCAT.md)

pub use hk_concat::{ConcatConfig, ConcatSession, concat_tools};
