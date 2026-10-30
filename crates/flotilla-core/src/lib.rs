//! flotilla's platform-neutral core, free of Tauri: `identity` says which document a URL is, and
//! `model` is the workspace of groups and tabs built on it (the one-copy rule and Recent). The
//! Tauri shell in `src-tauri` consumes both.

pub mod identity;
pub mod model;
pub mod nav;
pub mod session;
