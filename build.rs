//! Stamp the build with the commit it came from.
//!
//! So a stale `~/.local/bin/tmc` can say so. Installing is a separate step
//! from building, and a binary that silently predates the source is the kind
//! of thing you only notice by wondering why a feature you just wrote does
//! nothing.

use std::process::Command;

fn main() {
    let commit = Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=TMC_COMMIT={commit}");
    // Without this the stamp is baked in at first build and never refreshed.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
}
