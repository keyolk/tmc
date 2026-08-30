mod clock;
mod collect;
mod layout;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use layout::point;

#[derive(Parser)]
#[command(
    name = "tmc",
    about = "tmux workspace control: snapshot, diff, restore"
)]
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
    /// The hourly snapshot: write one per session, then prune.
    Autosave,
    /// Show the restore points on disk, newest first.
    List,
    /// Bring a restore point back.
    Load {
        /// Restore point name, e.g. `auto:20260829T204041Z` or `saved:mypoint`.
        /// Defaults to the newest.
        name: Option<String>,
        /// Restore only this session; repeatable.
        #[arg(long = "session", short)]
        sessions: Vec<String>,
        /// Print what would be created without touching tmux.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Save { name, dry_run } => save(name, dry_run),
        Command::Autosave => autosave(),
        Command::List => list(),
        Command::Load {
            name,
            sessions,
            dry_run,
        } => load(name, &sessions, dry_run),
    }
}

fn save(name: Option<String>, dry_run: bool) -> Result<()> {
    let Some(sessions) = snapshot_now()? else {
        println!("no tmux panes; nothing to save");
        return Ok(());
    };

    // A restore point is the whole workspace, not one session: saving them
    // separately made it impossible to bring a dashboard+projects pair back as
    // it was at one moment.
    let name = name.unwrap_or_else(|| clock::now().compact);
    if name == "autosave" {
        anyhow::bail!("'autosave' is reserved for the hourly snapshots");
    }
    let point = layout::save::layout_dir().join(name.replace('/', "_"));

    for session in &sessions {
        println!("  {}", describe(session));
        if !dry_run {
            let path = point.join(format!("{}.json", session.session.replace('/', "_")));
            layout::save::write(session, &path)?;
        }
    }

    let verb = if dry_run { "would write" } else { "saved" };
    println!(
        "\n{verb} {} session(s) -> {}",
        sessions.len(),
        point.display()
    );
    Ok(())
}

fn autosave() -> Result<()> {
    let now = clock::now();
    let root = layout::save::layout_dir().join("autosave");

    let Some(sessions) = snapshot_now()? else {
        // No server — e.g. after a reboot, before tmux runs. Not an error: the
        // job fires on a timer regardless.
        println!("{} autosave: no tmux server", now.timestamp);
        return Ok(());
    };

    let written = layout::autosave::write(&root, &sessions, &now.compact)?;
    let pruned_snapshots = layout::autosave::prune_old(&root, layout::autosave::KEEP);
    let pruned_sessions = layout::autosave::prune_dead(
        &root,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        layout::autosave::MAX_AGE_DAYS,
        layout::autosave::session_is_live,
    );

    // One line, because this goes to a log file that is read by tailing it.
    let mut line = format!(
        "{} autosave: {written} session(s) -> {}/<session>/{}.json",
        now.timestamp,
        root.display(),
        now.compact,
    );
    if pruned_snapshots > 0 {
        line.push_str(&format!(" (pruned {pruned_snapshots} snapshot(s)"));
        if pruned_sessions > 0 {
            line.push_str(&format!(", {pruned_sessions} dead session(s)"));
        }
        line.push(')');
    } else if pruned_sessions > 0 {
        line.push_str(&format!(" (pruned {pruned_sessions} dead session(s))"));
    }
    println!("{line}");
    Ok(())
}

fn list() -> Result<()> {
    let points = point::list(&layout::save::layout_dir());
    if points.is_empty() {
        println!("no restore points");
        return Ok(());
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    for p in &points {
        // The label answers "what was I working on", which the timestamp alone
        // never did — every hourly point otherwise looks identical. Files
        // written by tmux.sh have no label, so one is derived from the window
        // names on read rather than showing a blank column.
        let label = point::read(&p.reference)
            .ok()
            .map(|sessions| label_for(&sessions))
            .unwrap_or_default();
        println!(
            "{:<26} {:>9}  {} session(s)  {label}",
            p.name,
            clock::age_of(&p.sort_key, now_secs),
            p.sessions,
        );
    }
    Ok(())
}

/// A point's label: the stored one when present, else derived from the window
/// names across every session it holds.
fn label_for(sessions: &[layout::Session]) -> String {
    if let Some(stored) = sessions.iter().find_map(|s| s.label.as_deref()) {
        return stored.to_string();
    }
    const SHOWN: usize = 3;
    let names: Vec<&str> = sessions
        .iter()
        .flat_map(|s| &s.windows)
        .map(|w| w.name.as_str())
        .collect();
    let head = names
        .iter()
        .take(SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().saturating_sub(SHOWN) {
        0 => head,
        rest => format!("{head} +{rest}"),
    }
}

fn load(name: Option<String>, only: &[String], dry_run: bool) -> Result<()> {
    let points = point::list(&layout::save::layout_dir());
    let chosen = match &name {
        Some(wanted) => points
            .iter()
            .find(|p| &p.name == wanted || p.name.ends_with(wanted.as_str()))
            .with_context(|| format!("no restore point matching '{wanted}'"))?,
        None => points.first().context("no restore points on disk")?,
    };

    let sessions = point::read(&chosen.reference)?;
    let wanted: Vec<&layout::Session> = sessions
        .iter()
        .filter(|s| only.is_empty() || only.contains(&s.session))
        .collect();
    if wanted.is_empty() {
        anyhow::bail!(
            "restore point '{}' holds none of the requested sessions",
            chosen.name
        );
    }

    println!("restore point: {}", chosen.name);
    for s in &wanted {
        println!("  {}", describe(s));
    }
    println!();

    let mut total = layout::restore::Report::default();
    let mut server = layout::restore::Server;
    for s in &wanted {
        let report =
            layout::restore::session(&mut server, s, layout::restore::Selection::all(), dry_run)?;
        for note in &report.notes {
            println!("  {}: {note}", s.session);
        }
        total.windows += report.windows;
        total.panes += report.panes;
        total.missing_panes += report.missing_panes;
        total.commands_prefilled += report.commands_prefilled;
    }

    let verb = if dry_run { "would restore" } else { "restored" };
    println!(
        "\n{verb} {} window(s), {} pane(s), {} command(s) prefilled",
        total.windows, total.panes, total.commands_prefilled,
    );
    if total.missing_panes > 0 {
        println!("{} pane(s) did not fit the display", total.missing_panes);
    }
    Ok(())
}

/// Snapshot every live session, or `None` when there is no tmux server.
fn snapshot_now() -> Result<Option<Vec<layout::Session>>> {
    let panes = collect::tmux::panes().context("list tmux panes")?;
    if panes.is_empty() {
        return Ok(None);
    }
    let tree = collect::proc::Tree::capture_with_args().context("read process table")?;
    Ok(Some(layout::save::snapshot(
        &panes,
        &tree,
        &clock::now().timestamp,
    )))
}

fn describe(s: &layout::Session) -> String {
    let panes: usize = s.windows.iter().map(|w| w.panes.len()).sum();
    format!(
        "{}: {} windows, {} panes  [{}]",
        s.session,
        s.windows.len(),
        panes,
        label_for(std::slice::from_ref(s)),
    )
}
