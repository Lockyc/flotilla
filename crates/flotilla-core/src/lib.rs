//! flotilla's platform-neutral core, free of Tauri: `identity` says which document a URL is, and
//! `model` is the workspace of groups and tabs built on it (the one-copy rule and Recent), `nav`
//! decides what a navigation does to it, `route` picks the group a located document belongs
//! to, and `session` persists it. The Tauri shell in `src-tauri` consumes all five.

pub mod identity;
pub mod model;
pub mod nav;
pub mod route;
pub mod session;
