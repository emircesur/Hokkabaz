//! Public API modules scanned by codegen (`rust_input: crate::api`).
// frb attribute macros reference `cfg(frb_expand)`, unknown to rustc check-cfg.
#![allow(unexpected_cfgs)]

pub mod chat;
pub mod core;
pub mod flows;
pub mod instances;
pub mod settings;
