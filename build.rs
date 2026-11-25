//! Stamp the build with the commit it came from.
//!
//! So a stale `~/.local/bin/tmc` can say so. Installing is a separate step
//! from building, and a binary that silently predates the source is the kind
//! of thing you only notice by wondering why a feature you just wrote does
//! nothing.
//!
//! The release workflow sets `TMC_VERSION` to the tag it publishes, so a
//! downloaded binary reports `v0.1.3` rather than a bare commit. Its tags are
//! lightweight, which plain `git describe` ignores, and the staleness check in
//! `ui::model` compares against plain `git describe` output — so a local build
//! keeps the commit stamp instead of switching to `--tags`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=TMC_VERSION");
    let stamp = std::env::var("TMC_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(git_commit);

    println!("cargo:rustc-env=TMC_COMMIT={stamp}");
    // Without this the stamp is baked in at first build and never refreshed.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
}

fn git_commit() -> String {
    Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}
