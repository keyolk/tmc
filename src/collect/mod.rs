//! Collectors: everything that reads state from outside this process.
//!
//! Each one shells out and parses; none may hang the caller. `cmd::run`
//! enforces that with a timeout on every child.

pub mod cmd;
pub mod command;
pub mod proc;
pub mod tmux;
