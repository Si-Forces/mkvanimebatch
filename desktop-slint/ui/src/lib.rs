//! The host's generated Slint UI, alone in its crate: Slint's generated code expands to
//! `#[allow(unsafe_code)]`, which the workspace's `forbid` rejects (ADR-0017, 2026-10-23 amendment).
//! No hand-written code goes here.

slint::include_modules!();
