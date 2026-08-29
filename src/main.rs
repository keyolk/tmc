mod collect;
mod layout;

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tmxx", about = "tmux workspace time machine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a restore point for every session on the server.
    Save {
        /// Name for the restore point. Defaults to a UTC timestamp.
        name: Option<String>,
        /// Print what would be written without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Save { name, dry_run } => save(name, dry_run),
    }
}

fn save(name: Option<String>, dry_run: bool) -> Result<()> {
    let panes = collect::tmux::panes().context("list tmux panes")?;
    if panes.is_empty() {
        // No server, or no panes: nothing to snapshot. Not an error — the
        // hourly job runs whether or not tmux is up.
        println!("no tmux panes; nothing to save");
        return Ok(());
    }
    let tree = collect::proc::Tree::capture_with_args().context("read process table")?;

    let now = utc_now();
    let sessions = layout::save::snapshot(&panes, &tree, &now.timestamp);

    // A restore point is the whole workspace, not one session: saving them
    // separately made it impossible to bring a dashboard+projects pair back as
    // it was at one moment.
    let name = name.unwrap_or_else(|| now.compact.clone());
    if name == "autosave" {
        anyhow::bail!("'autosave' is reserved for the hourly snapshots");
    }
    let point = layout::save::layout_dir().join(name.replace('/', "_"));

    for session in &sessions {
        let path: PathBuf = point.join(format!("{}.json", session.session.replace('/', "_")));
        let panes: usize = session.windows.iter().map(|w| w.panes.len()).sum();
        println!(
            "  {}: {} windows, {} panes  [{}]",
            session.session,
            session.windows.len(),
            panes,
            session.label.as_deref().unwrap_or(""),
        );
        if !dry_run {
            layout::save::write(session, &path)?;
        }
    }

    if dry_run {
        println!(
            "\ndry run: {} session(s) would be written to {}",
            sessions.len(),
            point.display()
        );
    } else {
        println!(
            "\nsaved {} session(s) -> {}",
            sessions.len(),
            point.display()
        );
    }
    Ok(())
}

struct Now {
    /// `2026-08-30T00:00:00Z`, the `saved_at` format tmux.sh writes.
    timestamp: String,
    /// `20260830T000000Z`, used for directory names.
    compact: String,
}

/// Format the current UTC time without pulling in a date library.
///
/// Only two fixed formats are ever needed, and both are derived from the same
/// civil-time conversion below.
fn utc_now() -> Now {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    Now {
        timestamp: format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"),
        compact: format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z"),
    }
}

/// Days-from-civil, inverted. Howard Hinnant's algorithm — exact for every
/// date the epoch can express, and small enough to keep the dependency list at
/// what the collectors need.
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };

    (
        y,
        m,
        d,
        (rem / 3600) as u32,
        (rem % 3600 / 60) as u32,
        (rem % 60) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_known_instant() {
        // 2026-08-30T00:00:00Z, cross-checked with `date -u -r`.
        assert_eq!(civil_from_unix(1_788_048_000), (2026, 8, 30, 0, 0, 0));
        // The epoch itself.
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        // A leap day, which a naive 365-day conversion gets wrong.
        assert_eq!(civil_from_unix(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }

    #[test]
    fn formats_both_shapes_from_one_conversion() {
        let (y, mo, d, h, mi, s) = civil_from_unix(1_788_048_061);
        assert_eq!(
            format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"),
            "2026-08-30T00:01:01Z",
        );
    }
}
