//! The terminal UI.
//!
//! `model` holds what is shown and how it responds; `render` and `app` deal
//! with ratatui and the terminal. The split is what makes the interesting
//! behaviour testable without a terminal.

pub mod app;
pub mod clipboard;
mod commands;
mod drag;
mod guard;
pub mod keys;
mod link;
pub mod model;
mod mouse;
mod preview_links;
pub mod render;
mod selection;
pub mod snapshot;
pub mod state;
pub mod status;
pub mod theme;
mod worker;
