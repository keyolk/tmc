//! Small helpers for shelling out to `tmux` and `ps`.
//!
//! Every collector runs an external command; none of them may hang the UI.
//! `run` therefore always sets a timeout and never inherits stdin.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Run a command to completion and return stdout. Kills the child if it
/// outlives `timeout`, so one wedged child cannot freeze a refresh or a save.
///
/// Both pipes are drained on their own threads. Waiting for exit while the
/// child blocks writing into a full pipe buffer deadlocks — `ps -axo` on this
/// machine emits ~150 KB, well past the 64 KB pipe capacity.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {program}"))?;

    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };

    let stdout = stdout
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    match status {
        None => bail!("{program} timed out after {timeout:?}"),
        // Some tools (lsof) exit non-zero on partial results that are still
        // perfectly usable, so stdout wins when present.
        Some(status) if !status.success() && stdout.is_empty() => {
            bail!("{program} failed: {}", stderr.trim())
        }
        Some(_) => Ok(stdout),
    }
}

/// Read a pipe to EOF on its own thread.
fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buffer = String::new();
        // Non-UTF8 bytes in a command line must not lose the whole output.
        let mut raw = Vec::new();
        if pipe.read_to_end(&mut raw).is_ok() {
            buffer = String::from_utf8_lossy(&raw).into_owned();
        }
        buffer
    })
}

/// Default timeout for local, fast commands (`tmux`, `ps`).
pub const FAST: Duration = Duration::from_secs(3);
