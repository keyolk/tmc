//! The terminal UI.
//!
//! `model` holds what is shown and how it responds; `render` and `app` deal
//! with ratatui and the terminal. The split is what makes the interesting
//! behaviour testable without a terminal.

pub mod app;
pub mod clipboard;
mod commands;
mod guard;
pub mod keys;
pub mod model;
mod mouse;
pub mod render;
pub mod snapshot;
pub mod state;
pub mod status;
pub mod theme;
mod worker;
