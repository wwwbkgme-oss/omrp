//! RTK filter implementations.
//!
//! Each submodule exports a `compress(input: &str) -> String` function
//! plus relevant constants.

pub mod build_output;
pub mod dedup_log;
pub mod find;
pub mod git_diff;
pub mod git_status;
pub mod grep;
pub mod ls;
pub mod smart_truncate;
pub mod tree;
