mod clock;
mod collect;
mod fuzzy;
mod layout;
mod ui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use layout::point;

#[derive(Parser)]
#[command(
    name = "tmc",
    about = "tmux workspace control: snapshot, diff, restore"
)]
struct Cli {
    /// No subcommand opens the TUI — the common case, and what the `w` key
    /// binding runs.
    #[command(subcommand)]
    command: Option<Command>,
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
    Autosave {
        /// Skip the write when nothing has changed since the last snapshot.
        /// Lets tmux hooks fire this on every window change without filling
        /// the disk with identical points.
        #[arg(long)]
        if_drifted: bool,
    },
    /// Check a restore point for problems before you need it.
    Doctor {
        /// Restore point to check. Defaults to checking every one.
        name: Option<String>,
    },
    /// Pick a tmux paste buffer and paste it.
    Clipboard {
        /// Pane to paste into. Defaults to $TMUX_PANE, the pane that ran this.
        #[arg(long)]
        target: Option<String>,
    },
    /// Send a copy-mode command to a pane.
    CopyMode {
        /// Pane to act on. Defaults to $TMUX_PANE.
        #[arg(long)]
        target: Option<String>,
    },
    /// Render one TUI frame as text, for reviewing the layout.
    Snapshot {
        #[arg(long, default_value = "100")]
        width: u16,
        #[arg(long, default_value = "24")]
        height: u16,
    },
    /// Show what has changed since a restore point.
    Diff {
        /// Restore point to compare against. Defaults to the newest.
        name: Option<String>,
        /// List unchanged windows too.
        #[arg(long)]
        all: bool,
    },
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
    let Some(command) = Cli::parse().command else {
        return tui();
    };
    match command {
        Command::Save { name, dry_run } => save(name, dry_run),
        Command::Autosave { if_drifted } => autosave(if_drifted),
        Command::List => list(),
        Command::Diff { name, all } => diff(name, all),
        Command::Snapshot { width, height } => snapshot(width, height),
        Command::Doctor { name } => doctor(name),
        Command::Clipboard { target } => ui::clipboard::run(&target.unwrap_or_else(current_pane)),
        Command::CopyMode { target } => copy_mode(&target.unwrap_or_else(current_pane)),
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

fn autosave(if_drifted: bool) -> Result<()> {
    let now = clock::now();
    let root = layout::save::layout_dir().join("autosave");

    let panes = collect::tmux::panes().context("list tmux panes")?;
    if panes.is_empty() {
        // No server — e.g. after a reboot, before tmux runs. Not an error: the
        // job fires on a timer regardless.
        println!("{} autosave: no tmux server", now.timestamp);
        return Ok(());
    }
    let tree = collect::proc::Tree::capture_with_args().context("read process table")?;
    let sessions = layout::save::snapshot(&panes, &tree, &now.timestamp);

    if if_drifted && !drifted_since_last(&root, &panes, &tree)? {
        println!("{} autosave: no drift; skipped", now.timestamp);
        return Ok(());
    }

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

/// Open the TUI, then act on what it chose.
///
/// The switch happens here rather than inside the loop: tmux's `select-window`
/// while the alternate screen is still up leaves the terminal in a state the
/// restored screen then paints over.
fn tui() -> Result<()> {
    match ui::app::run()? {
        ui::app::Outcome::Quit => Ok(()),
        ui::app::Outcome::Switch(target) => {
            let (session, _) = target.split_once(':').unwrap_or((&target, ""));
            collect::cmd::run(
                "tmux",
                &["select-window", "-t", &target],
                collect::cmd::FAST,
            )?;
            // Only needed when the target lives in another session; harmless
            // otherwise, and cheaper than asking which session is attached.
            let _ = collect::cmd::run(
                "tmux",
                &["switch-client", "-t", session],
                collect::cmd::FAST,
            );
            Ok(())
        }
    }
}

/// Whether the live workspace differs from the newest autosave.
///
/// Uses the same comparison the `diff` command shows, deliberately. An earlier
/// version compared the serialized windows and treated any difference as
/// drift, which fired on every run: snapshots written by tmux.sh carry none of
/// tmc's fields (`pane_id`, `shell_only`, `session_confidence`), so a workspace
/// nobody had touched still looked changed. Drift has to mean what the user
/// would recognize as a change, not what the file format happens to record.
fn drifted_since_last(
    root: &std::path::Path,
    live: &[collect::tmux::Pane],
    tree: &collect::proc::Tree,
) -> Result<bool> {
    let points = point::list(root.parent().unwrap_or(root));
    let Some(previous) = points.iter().find(|p| p.is_auto) else {
        return Ok(true); // nothing to compare against yet
    };
    let saved = point::read(&previous.reference)?;
    Ok(layout::diff::compare(live, &saved, tree).has_drift())
}

fn diff(name: Option<String>, all: bool) -> Result<()> {
    let points = point::list(&layout::save::layout_dir());
    let chosen = match &name {
        Some(wanted) => points
            .iter()
            .find(|p| &p.name == wanted || p.name.ends_with(wanted.as_str()))
            .with_context(|| format!("no restore point matching '{wanted}'"))?,
        None => points.first().context("no restore points on disk")?,
    };
    let saved = point::read(&chosen.reference)?;

    let panes = collect::tmux::panes().context("list tmux panes")?;
    let tree = collect::proc::Tree::capture_with_args().context("read process table")?;
    let diff = layout::diff::compare(&panes, &saved, &tree);

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (modified, added, removed) = diff.counts();
    println!(
        "{}  ({})\n{modified} changed, {added} added, {removed} removed\n",
        chosen.name,
        clock::age_of(&chosen.sort_key, now_secs),
    );

    for w in &diff.windows {
        if w.change == layout::diff::Change::Same && !all {
            continue;
        }
        println!("{} {:<24} {}", w.change.marker(), w.target(), w.name);
        for reason in &w.reasons {
            println!("      {reason}");
        }
    }

    if !diff.has_drift() {
        println!("no changes");
    }
    Ok(())
}

/// Render one frame at a fixed size. How the layout is reviewed without an
/// interactive terminal.
fn snapshot(width: u16, height: u16) -> Result<()> {
    let points = point::list(&layout::save::layout_dir());
    let mut model = ui::model::Model::new(points);

    let panes = collect::tmux::panes().unwrap_or_default();
    let tree = collect::proc::Tree::capture_with_args()?;
    let pending = collect::notify::load();
    let saved = match model.current_point() {
        Some(p) => point::read(&p.reference).unwrap_or_default(),
        None => Vec::new(),
    };
    model.refresh(&panes, &saved, &tree, &pending);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("{}", ui::snapshot::render(&model, width, height, now));
    Ok(())
}

/// The pane this process was launched from.
///
/// `$TMUX_PANE` rather than asking tmux for the active pane: a popup runs in
/// its own pane, so `display-message -p` would name the popup, not the pane
/// the user was looking at.
fn current_pane() -> String {
    std::env::var("TMUX_PANE").unwrap_or_default()
}

/// List the copy-mode commands and send the chosen one.
///
/// Plain output rather than a TUI: the list is short and fixed, and this is
/// invoked from a popup that a picker would nest inside.
fn copy_mode(target: &str) -> Result<()> {
    use collect::clipboard::COPY_COMMANDS;

    let width = COPY_COMMANDS
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(0);
    for c in COPY_COMMANDS {
        println!("{:<width$}  {}", c.name, c.about);
    }
    eprintln!();
    eprint!("command: ");

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let name = line.trim();
    if name.is_empty() {
        return Ok(());
    }
    if !COPY_COMMANDS.iter().any(|c| c.name == name) {
        anyhow::bail!("unknown copy-mode command: {name}");
    }
    collect::clipboard::send_copy_command(name, target)
}

fn doctor(name: Option<String>) -> Result<()> {
    let points = point::list(&layout::save::layout_dir());
    if points.is_empty() {
        println!("no restore points");
        return Ok(());
    }

    let chosen: Vec<&point::Point> = match &name {
        Some(wanted) => points
            .iter()
            .filter(|p| &p.name == wanted || p.name.ends_with(wanted.as_str()))
            .collect(),
        None => points.iter().collect(),
    };
    if chosen.is_empty() {
        anyhow::bail!("no restore point matching '{}'", name.unwrap_or_default());
    }

    let detailed = chosen.len() == 1;
    let mut unhealthy = 0;

    for p in chosen {
        let sessions = match point::read(&p.reference) {
            Ok(s) => s,
            Err(e) => {
                println!("{:<26} unreadable: {e}", p.name);
                unhealthy += 1;
                continue;
            }
        };
        let findings = layout::doctor::check(&sessions);
        let verdict = layout::doctor::summarize(&findings);
        if !findings.is_empty() {
            unhealthy += 1;
        }
        println!("{:<26} {verdict}", p.name);

        // One point: show every finding. All of them: a line each, or the
        // output buries the answer under 28 points' worth of detail.
        if detailed {
            for f in &findings {
                let tag = match f.severity {
                    layout::doctor::Severity::Warn => "warn",
                    layout::doctor::Severity::Note => "note",
                };
                println!("  {tag}  {:<20} {}", f.where_, f.what);
            }
        }
    }

    if !detailed {
        println!("\n{unhealthy} point(s) with findings; name one to see them");
    }
    Ok(())
}
