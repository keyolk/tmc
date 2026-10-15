//! The terminal loop: poll, redraw only on change, handle keys.
//!
//! The loop owns the terminal and the model and never shells out. Reloads,
//! previews and the commands that change tmux run on `worker` threads and
//! come back as replies; the loop drains them between keys, so Esc answers
//! while a restore is still bringing windows up.

use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use super::commands::{cancel_move, confirm_move, restore_marked, run_action, save_point};
use super::keys::{self, Cmd, Ctx, Key};
use super::model::{Model, actions};
use super::state;
use super::worker::{Job, Reply, Snapshot, Workers};
use crate::layout::point::{self, Ref};
use crate::layout::save;

/// How often the world is re-read. tmux exposes no event stream a foreign
/// process can subscribe to, so this polls; `fingerprint` keeps the redraw
/// from happening when nothing moved.
const TICK: Duration = Duration::from_secs(2);

/// How often the loop looks for a worker's reply while one is outstanding.
/// crossterm cannot wait on a channel and the terminal at once, so the loop
/// polls the terminal in short slices until the reply lands — and only then:
/// with nothing outstanding it sleeps until the next tick.
const REPLY_POLL: Duration = Duration::from_millis(15);

/// What the caller should do once the terminal is restored.
pub enum Outcome {
    Quit,
    /// Switch to this tmux target, e.g. `projects:3`.
    Switch(String),
}

/// Open the TUI.
///
/// `search` decides which mode it starts in. Searching is the default,
/// because this runs as a popup: summoning it is already the decision to go
/// somewhere, and making you press `/` first is the same extra keystroke that
/// made tmux-fzf's two-level menu tiresome. `Esc` steps out to the tree when
/// the intent is to inspect rather than jump, and again to leave.
pub fn run(search: bool) -> Result<Outcome> {
    let points = point::list(&save::layout_dir());
    let mut model = Model::new(points);
    model.theme = super::theme::Theme::detect();
    model.searching = search;

    // The last session's expanded windows, query and point. A broken file is
    // said, not fatal: it is replaced on the way out.
    let state_path = state::path();
    match state::load(&state_path) {
        Ok(saved) => saved.restore(&mut model),
        Err(e) => model.status.error(format!("{e:#}")),
    }

    let focus = summoning_window();
    let mut terminal = setup().context("enter alternate screen")?;
    let result = event_loop(&mut terminal, &mut model, focus);
    teardown(&mut terminal)?;

    // After the terminal is back, so a failure can be printed where it will
    // be read, and so the write never holds up a keypress.
    if let Err(e) = state::save(&state_path, &state::UiState::capture(&model)) {
        eprintln!("tmc: could not save UI state: {e:#}");
    }
    result?;

    Ok(match model.switch_to {
        Some(target) => Outcome::Switch(target),
        None => Outcome::Quit,
    })
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn setup() -> Result<Term> {
    restore_terminal_on_panic();
    enable_raw_mode()?;
    let mut out = io::stdout();
    crossterm::execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

/// Put the terminal back before a panic message is printed.
///
/// A panic unwinds past `teardown`, so without this the shell is left in raw
/// mode on the alternate screen: the message is drawn into a screen that is
/// about to vanish, and the popup's pane is unusable until `reset`. The
/// previous hook still runs afterwards, so the message and backtrace are
/// unchanged -- they just land on the normal screen.
///
/// Only for the input thread. A worker's panic is caught and shown in the
/// status line while the UI carries on; restoring the terminal for it would
/// drop the alternate screen out from under a running TUI, and printing it
/// would scribble over the frame.
fn restore_terminal_on_panic() {
    let ui = std::thread::current().id();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() != ui {
            return;
        }
        // A pane's render is guarded and the TUI carries on after it: keep
        // the terminal, and leave the message to the guard's log rather than
        // printing it over the frame.
        if super::guard::active() {
            super::guard::note(info);
            return;
        }
        // Errors are ignored on purpose: this is already the failure path, and
        // there is nowhere left to report a terminal that will not reset.
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        previous(info);
    }));
}

fn teardown(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// What a key asks of the loop beyond changing the model.
pub(super) enum Effect {
    None,
    /// Re-read the world (a different restore point, or `R`).
    Reload,
    /// Rebuild the tree from the last reading: expanding a window changes
    /// which rows exist, not what tmux says.
    Rebuild,
    /// Run a command on the worker.
    Job(Job),
}

/// The loop's side of the workers: what is outstanding and what came back.
struct Io {
    workers: Workers,
    /// The newest reload or command requested. A reload reply from an older
    /// one is superseded and dropped.
    generation: u64,
    /// Reload replies still to come. The tick starts no reload while one is
    /// out, so a slow `ps` cannot stack a queue of them.
    reloads: usize,
    /// The restore point the running command's re-read will compare against.
    job_point: Option<Ref>,
    /// The pane capture asked for and not yet answered.
    preview: Option<String>,
    /// The last good reading, for `Effect::Rebuild`.
    snapshot: Option<Snapshot>,
    /// The reload error on screen, so a later good reload can take it down.
    reload_error: Option<String>,
    /// Where the cursor goes once the first reading lands.
    focus: Option<String>,
}

impl Io {
    fn new(focus: Option<String>) -> Self {
        Self {
            workers: Workers::start(),
            generation: 0,
            reloads: 0,
            job_point: None,
            preview: None,
            snapshot: None,
            reload_error: None,
            focus,
        }
    }

    fn outstanding(&self, model: &Model) -> bool {
        self.reloads > 0 || self.preview.is_some() || model.busy.is_some()
    }

    fn reload(&mut self, model: &Model) {
        // A running command re-reads the world when it ends; a reload queued
        // behind it would only supersede that reading.
        if model.busy.is_some() {
            return;
        }
        self.generation += 1;
        self.reloads += 1;
        self.workers.reload(
            self.generation,
            model.current_point().map(|p| p.reference.clone()),
        );
    }

    fn run(&mut self, model: &mut Model, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Reload => self.reload(model),
            Effect::Rebuild => match &self.snapshot {
                Some(s) => s.apply(model),
                None => self.reload(model),
            },
            Effect::Job(job) => {
                self.generation += 1;
                model.busy = Some(job.label);
                self.job_point = model.current_point().map(|p| p.reference.clone());
                self.workers
                    .job(self.generation, self.job_point.clone(), job);
            }
        }
    }

    /// Ask for the capture the panel needs, one at a time. A held-down `j`
    /// would otherwise queue a `capture-pane` per row it passes.
    fn want_preview(&mut self, model: &mut Model) {
        let Some(target) = preview_target(model) else {
            model.preview = None;
            return;
        };
        if self.preview.is_some() || model.preview_for(&target).is_some() {
            return;
        }
        self.preview = Some(target.clone());
        self.workers.capture(target);
    }

    /// Take one reply into the model. Returns whether the screen changed.
    fn apply(&mut self, model: &mut Model, reply: Reply) -> bool {
        match reply {
            Reply::Reloaded {
                generation,
                snapshot,
            } => {
                self.reloads = self.reloads.saturating_sub(1);
                if generation != self.generation {
                    return false;
                }
                // Not while the `x` menu is up. It names what it would act on
                // -- "2 panes", "3 windows" -- and a reload landing between
                // the prompt and the keypress would move the cursor or drop a
                // mark, so the command would act on something other than what
                // was on screen. The next tick reads again.
                if model.menu {
                    return false;
                }
                let before = (model.fingerprint(), model.status.clone());
                self.settle(model, snapshot);
                before != (model.fingerprint(), model.status.clone())
            }
            Reply::JobDone {
                generation,
                done,
                snapshot,
            } => {
                model.busy = None;
                let mut used = self.job_point.take();
                if let Ok(d) = &done
                    && let Some(points) = &d.points
                {
                    used = points.first().map(|p| p.reference.clone());
                    model.points = points.clone();
                    model.point_index = 0;
                }
                if generation == self.generation {
                    self.settle(model, snapshot);
                }
                match done {
                    Ok(d) => (d.finish)(model),
                    Err(e) => model.status.error(format!("{e:#}")),
                }
                // `p` pressed while the command ran: its re-read compared
                // against the old point.
                if model.current_point().map(|p| &p.reference) != used.as_ref() {
                    self.reload(model);
                }
                true
            }
            Reply::Preview { target, body } => {
                self.preview = None;
                if preview_target(model).as_deref() == Some(target.as_str()) {
                    model.preview = Some((target, body));
                    true
                } else {
                    // The cursor moved on while this was captured.
                    self.want_preview(model);
                    false
                }
            }
        }
    }

    fn settle(&mut self, model: &mut Model, snapshot: Result<Snapshot>) {
        match snapshot {
            Ok(s) => {
                s.apply(model);
                self.snapshot = Some(s);
                settle_reload(model, Ok(()), &mut self.reload_error);
                // The window this was summoned from is the context the user
                // is already in, so it is what the panel describes first.
                if let Some(target) = self.focus.take() {
                    model.focus(&target);
                }
            }
            Err(e) => settle_reload(model, Err(e), &mut self.reload_error),
        }
    }
}

fn event_loop(terminal: &mut Term, model: &mut Model, focus: Option<String>) -> Result<()> {
    // Paint the chrome before reading the world. This runs as a popup, and the
    // first reload costs ~0.1s of `ps` and `tmux` before it can say anything;
    // the frame drawn here is the real one with an empty tree, so the reply
    // fills the rows in rather than replacing the screen.
    terminal.draw(|frame| super::render::draw(frame, model, now_secs()))?;

    let mut io = Io::new(focus);
    io.reload(model);
    let mut needs_redraw = false;
    let mut next_tick = Instant::now() + TICK;

    loop {
        if needs_redraw {
            io.want_preview(model);
            let now = now_secs();
            terminal.draw(|frame| super::render::draw(frame, model, now))?;
            needs_redraw = false;
        }

        let now = Instant::now();
        let until_tick = next_tick.saturating_duration_since(now);
        let wait = if io.outstanding(model) {
            until_tick.min(REPLY_POLL)
        } else {
            until_tick
        };
        // A status that times out wakes the loop once, at its deadline. Not a
        // timer: with nothing on the status line this adds no wakeups at all.
        let wait = match model.status.deadline() {
            Some(at) => wait.min(at.saturating_duration_since(now)),
            None => wait,
        };
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let effect = handle_key(model, key.code, key.modifiers);
                    io.run(model, effect);
                    if model.quit || model.switch_to.is_some() {
                        return finish_running_command(terminal, model, &mut io);
                    }
                    needs_redraw = true;
                }
                Event::Key(_) => {}
                _ => needs_redraw = true, // resize
            }
        }

        while let Some(reply) = io.workers.try_recv() {
            needs_redraw |= io.apply(model, reply);
        }

        // The only redraw expiry costs: true once, when the message goes.
        needs_redraw |= model.status.expire(Instant::now());

        // Tick: re-read the world. The reply redraws only when the display
        // would differ, so an idle workspace stays at 0 fps.
        if Instant::now() >= next_tick {
            next_tick = Instant::now() + TICK;
            if io.reloads == 0 && !model.menu {
                io.reload(model);
            }
        }
    }
}

/// Leaving while a command runs waits for it.
///
/// The process exiting would kill the worker mid-command: a restore stopped
/// between `new-window` and `send-keys` leaves windows with no command in
/// them, a merge stops with half the panes moved. Every tmux call has its own
/// timeout, so this wait is bounded.
fn finish_running_command(terminal: &mut Term, model: &mut Model, io: &mut Io) -> Result<()> {
    let Some(label) = model.busy else {
        return Ok(());
    };
    model.status.info(format!("waiting for {label} to finish…"));
    terminal.draw(|frame| super::render::draw(frame, model, now_secs()))?;
    while model.busy.is_some() {
        let Some(reply) = io.workers.recv() else {
            break; // the worker is gone; nothing left to wait for
        };
        io.apply(model, reply);
    }
    Ok(())
}

/// Turn a key into a change to the model, through the binding table.
pub(super) fn handle_key(model: &mut Model, code: KeyCode, mods: KeyModifiers) -> Effect {
    model.status.on_key(Instant::now());
    let ctx = keys::context(model);

    // Under a Korean input source the shortcut keys arrive as jamo (`q` -> `ㅂ`)
    // and do nothing until the input source is switched back. Rewrite them to
    // the Latin key at the same physical position -- but not where letters
    // type, since there the jamo IS the input.
    let code = if ctx.types() {
        code
    } else {
        crate::keymap::normalize(code, mods)
    };
    let Some(key) = Key::from_event(code, mods) else {
        return Effect::None;
    };

    // A restored query is shown selected until the first key on the search
    // line decides what happens to it. Any other key accepts it as it is.
    if ctx == Ctx::Search && std::mem::take(&mut model.search_selected) {
        match key {
            Key::Char(_) | Key::Backspace => model.search_clear(),
            _ => {}
        }
        if key == Key::Backspace {
            return Effect::None;
        }
    }

    // The menu is one-shot by construction: closed before anything is
    // matched, so Esc, an unknown letter and a command that ran all leave it
    // closed without an arm each.
    if ctx == Ctx::Menu {
        model.menu = false;
    }

    match keys::lookup(ctx, key) {
        Some(binding) => run_cmd(model, binding.cmd),
        None => {
            unbound(model, ctx, key);
            Effect::None
        }
    }
}

/// A key no binding claims: typing where letters type, a reminder where
/// only a few keys mean anything, silence elsewhere.
fn unbound(model: &mut Model, ctx: Ctx, key: Key) {
    match (ctx, key) {
        (Ctx::Search, Key::Char(c)) => model.search_push(c),
        (Ctx::MoveSearch, Key::Char(c)) => {
            model.search_push(c);
            // The filter moved the rows under the cursor; land it back on a
            // window that can actually receive the panes.
            model.move_destination(0);
        }
        (Ctx::Move, _) => {
            let hint = keys::footer(Ctx::Move, model, usize::MAX);
            model.status.warn(format!("choose a window: {hint}"));
        }
        // An unknown letter after `x` is a slip, and a complaint about it
        // would be noise on the screen that was showing the answer.
        _ => {}
    }
}

/// Whether a command changes tmux or the points, and so runs on the worker.
fn uses_worker(cmd: Cmd) -> bool {
    use super::model::ActionKind;
    matches!(
        cmd,
        Cmd::Save
            | Cmd::Restore
            | Cmd::Confirm
            | Cmd::Run(ActionKind::Break | ActionKind::Send | ActionKind::Kill)
    )
}

fn run_cmd(model: &mut Model, cmd: Cmd) -> Effect {
    // One command at a time. Two would race on the same windows, and the
    // second was chosen against a tree the first is busy changing.
    if uses_worker(cmd)
        && let Some(label) = model.busy
    {
        model
            .status
            .warn(format!("still {label} — try again when it finishes"));
        return Effect::None;
    }

    let job = |j: Option<Job>| j.map_or(Effect::None, Effect::Job);
    match cmd {
        Cmd::Quit => model.quit = true,
        Cmd::Search => model.searching = true,
        Cmd::ToTree | Cmd::FilterOff => model.searching = false,
        Cmd::SearchPop => {
            model.search_pop();
            if model.pending_move.is_some() {
                model.move_destination(0);
            }
        }
        Cmd::SearchClear => model.search_clear(),
        Cmd::Down => model.move_cursor(1),
        Cmd::Up => model.move_cursor(-1),
        Cmd::Top => {
            model.cursor = 0;
            model.move_cursor(0);
        }
        Cmd::Bottom => {
            model.cursor = model.rows.len().saturating_sub(1);
            model.move_cursor(-1);
        }
        Cmd::Switch => {
            model.searching = false;
            if let Some(w) = model.current_window() {
                if w.gone {
                    // Nothing to switch to; the window only exists in the point.
                    model
                        .status
                        .warn("that window is gone — press r to restore it");
                } else {
                    model.switch_to = Some(w.target());
                }
            }
        }
        Cmd::Mark => model.toggle_mark(),
        Cmd::MarkAll => model.mark_all_changed(),
        Cmd::ClearMarks => model.clear_marks(),
        Cmd::NextWaiting => model.jump_waiting(),
        // p/P walk the restore points; the diff follows.
        Cmd::PointNext if model.cycle_point(1) => return Effect::Reload,
        Cmd::PointPrev if model.cycle_point(-1) => return Effect::Reload,
        Cmd::PointNext | Cmd::PointPrev => {}
        Cmd::Save => return Effect::Job(save_point()),
        Cmd::Restore => return job(restore_marked(model)),
        Cmd::Expand if model.set_expanded(Some(true)) => return Effect::Rebuild,
        Cmd::Collapse if model.set_expanded(Some(false)) => return Effect::Rebuild,
        Cmd::Expand | Cmd::Collapse => {}
        Cmd::Menu => model.menu = true,
        Cmd::CloseMenu => model.menu = false,
        Cmd::Run(kind) => match actions(model).into_iter().find(|a| a.kind == kind) {
            Some(action) if action.enabled => return job(run_action(model, kind)),
            // A greyed row already says what is missing; pressing it should
            // say the same thing rather than close in silence.
            Some(action) => model
                .status
                .warn(format!("{} — {}", action.label, action.detail)),
            None => {}
        },
        Cmd::Reload => return Effect::Reload,
        Cmd::Help => {
            model.help = true;
            model.help_scroll = 0;
        }
        Cmd::CloseHelp => model.help = false,
        // Clamped by the renderer, which knows how tall the box is.
        Cmd::HelpDown => model.help_scroll += 1,
        Cmd::HelpUp => model.help_scroll = model.help_scroll.saturating_sub(1),
        Cmd::DestDown => {
            model.move_destination(1);
        }
        Cmd::DestUp => {
            model.move_destination(-1);
        }
        Cmd::Confirm => return job(confirm_move(model)),
        Cmd::CancelMove => cancel_move(model),
    }
    Effect::None
}

/// Put a reload's outcome on screen. A failure keeps the last good tree and
/// says why; the next success takes that message down, but only if it is
/// still the one showing -- a status the user caused since then stays.
fn settle_reload(model: &mut Model, result: Result<()>, shown: &mut Option<String>) {
    match result {
        Ok(()) => {
            if let Some(msg) = shown.take()
                && model.status.text() == msg
            {
                model.status.clear();
            }
        }
        Err(e) => {
            let msg = format!("reload failed: {e:#}");
            model.status.error(msg.clone());
            *shown = Some(msg);
        }
    }
}

/// The pane the panel previews: a pane row previews that pane, a window row
/// its active one. Without this, expanding a window of three identical
/// claude commands shows the same output whichever you select — and the
/// whole reason to expand is to tell them apart.
fn preview_target(model: &Model) -> Option<String> {
    match (model.current_pane(), model.current_window()) {
        (Some(p), _) => Some(p.target().to_string()),
        (None, Some(w)) if !w.gone => Some(w.target()),
        _ => None,
    }
}
/// The window that ran this, as `session:index`.
///
/// Public so `tmc snapshot` renders the same frame the TUI opens on — a
/// snapshot that skipped the focus would review a layout nobody sees.
///
/// Asked of tmux rather than derived from `$TMUX_PANE`, because a popup is
/// itself a pane and `display-message` without a target would name the popup.
/// `-t $TMUX_PANE` pins the answer to the pane the key was pressed in.
pub fn summoning_window() -> Option<String> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    let out = crate::collect::cmd::run(
        "tmux",
        &[
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{session_name}:#{window_index}",
        ],
        crate::collect::cmd::FAST,
    )
    .ok()?;
    let target = out.trim().to_string();
    (!target.is_empty()).then_some(target)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::diff::Change;
    use crate::ui::commands::{join_pane_args, selected_windows};
    use crate::ui::model::{ActionKind, MoveKind, Row, WindowRow};
    use crate::ui::worker::Done;

    fn model_with_window() -> Model {
        let mut m = Model::new(Vec::new());
        m.rows = vec![Row::Window(WindowRow {
            session: "projects".into(),
            index: 1,
            name: "alpha".into(),
            panes: 2,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            running_claude: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        })];
        m
    }

    /// Keys that only read state, so they can be exercised without touching
    /// tmux. The mutating ones (m/b/J/x/r/s) shell out and are covered by
    /// their own modules.
    fn press(model: &mut Model, c: char, mods: KeyModifiers) {
        let _ = handle_key(model, KeyCode::Char(c), mods);
    }

    #[test]
    fn a_failed_reload_keeps_the_tree_and_says_why() {
        let mut m = model_with_window();
        let mut shown = None;
        settle_reload(&mut m, Err(anyhow::anyhow!("ps timed out")), &mut shown);

        assert_eq!(m.rows.len(), 1, "the last good tree stays on screen");
        assert!(m.status.contains("ps timed out"), "{}", m.status);
        assert!(!m.quit, "a transient failure does not close the popup");

        settle_reload(&mut m, Ok(()), &mut shown);
        assert!(m.status.is_empty(), "recovery takes the error down");
    }

    #[test]
    fn recovery_leaves_a_newer_status_alone() {
        let mut m = model_with_window();
        let mut shown = None;
        settle_reload(&mut m, Err(anyhow::anyhow!("tmux gone")), &mut shown);
        m.status.success("saved 2 session(s)");

        settle_reload(&mut m, Ok(()), &mut shown);
        assert_eq!(m.status, "saved 2 session(s)");
    }

    #[test]
    fn esc_leaves_search_for_normal_mode_without_quitting() {
        let mut m = model_with_window();
        m.searching = true;
        for c in "alpha".chars() {
            m.search_push(c);
        }

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.searching, "Esc returns to normal mode");
        assert_eq!(m.search, "alpha", "the query remains as a filter");
        assert!(!m.quit, "the first Esc is a mode change, not an exit");
    }

    #[test]
    fn esc_in_the_tree_quits() {
        // One level per press. The tree is the last one, so Esc there leaves.
        let mut m = model_with_window();
        assert!(!m.searching, "already at the root");

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);

        assert!(m.quit);
    }

    #[test]
    fn two_escapes_from_the_search_line_leave() {
        // The path the TUI actually opens on: search -> tree -> out.
        let mut m = model_with_window();
        m.searching = true;

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.quit, "the first lands in the tree");
        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(m.quit, "the second leaves");
    }

    #[test]
    fn esc_during_a_pane_move_cancels_rather_than_quitting() {
        // The level below the tree still has to be popped first, or a cancel
        // costs the whole session.
        let mut m = model_with_window();
        m.pending_move = Some(crate::ui::model::PendingMove {
            panes: vec![("%1084".into(), "projects:2".into())],
            kind: MoveKind::Panes,
            destination: None,
        });

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.quit, "the move was cancelled, not the program");

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(m.quit, "and the next press is at the root");
    }

    #[test]
    fn tab_hands_the_keys_to_the_tree_but_keeps_the_filter() {
        // The single-key commands live in the tree; the filter is what got you
        // to the right handful of windows, so it survives the switch.
        let mut m = model_with_window();
        m.searching = true;
        for c in "alp".chars() {
            m.search_push(c);
        }

        let _ = handle_key(&mut m, KeyCode::Tab, KeyModifiers::NONE);
        assert!(!m.searching);
        assert_eq!(m.search, "alp", "the filter is still applied");

        // And a letter is a command again rather than search input.
        let _ = handle_key(&mut m, KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(m.marks.len(), 1, "space marked instead of typing");
    }

    #[test]
    fn typing_in_search_mode_does_not_trigger_commands() {
        // `s` saves and `x` kills a window in the tree. Landing on the search
        // line means a stray keystroke cannot do either.
        let mut m = model_with_window();
        m.searching = true;
        for c in "sx".chars() {
            let _ = handle_key(&mut m, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(m.search, "sx");
        assert!(m.marks.is_empty());
        assert!(!m.quit);
    }

    #[test]
    fn control_chords_move_instead_of_typing() {
        // A `Char(c)` catch-all placed before these swallowed them and typed
        // the letter — clippy caught it as an unreachable arm, but the visible
        // symptom would have been `n` appearing in the query.
        let mut m = Model::new(Vec::new());
        m.rows = vec![
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 1,
                name: "alpha".into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                running_claude: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            }),
            Row::Window(WindowRow {
                session: "projects".into(),
                index: 2,
                name: "beta".into(),
                panes: 1,
                state: String::new(),
                cc_session: String::new(),
                waiting: false,
                running_claude: false,
                change: Change::Same,
                reasons: Vec::new(),
                gone: false,
            }),
        ];
        m.searching = true;

        let _ = handle_key(&mut m, KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert!(m.search.is_empty(), "nothing was typed");
        assert_eq!(
            m.current_window().map(|w| w.name.clone()),
            Some("beta".into())
        );

        let _ = handle_key(&mut m, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(
            m.current_window().map(|w| w.name.clone()),
            Some("alpha".into())
        );
    }

    #[test]
    fn pane_move_names_the_exact_source_and_destination() {
        assert_eq!(
            join_pane_args("%1084", "tooling:3"),
            ["join-pane", "-d", "-s", "%1084", "-t", "tooling:3"],
        );
    }

    #[test]
    fn esc_cancels_a_pane_move_without_quitting() {
        let mut m = model_with_window();
        m.pending_move = Some(pending_move());

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);

        assert!(m.pending_move.is_none(), "the pending move is gone");
        assert!(!m.quit, "Esc cancels the mode, not the program");
        assert_eq!(m.status, "pane move cancelled");
    }

    /// Two live windows, so the destination picker has something to offer.
    fn model_with_two_windows() -> Model {
        let mut m = model_with_window();
        m.rows.push(Row::Window(WindowRow {
            session: "projects".into(),
            index: 2,
            name: "beta".into(),
            panes: 1,
            state: String::new(),
            cc_session: String::new(),
            waiting: false,
            running_claude: false,
            change: Change::Same,
            reasons: Vec::new(),
            gone: false,
        }));
        m
    }

    #[test]
    fn merge_says_what_it_needs_rather_than_guessing() {
        // One window is not a merge, and picking a second for the user would
        // be picking which of 28 to close.
        let mut m = model_with_two_windows();
        m.marks.insert("projects:1".into());

        press(&mut m, 'x', KeyModifiers::NONE);
        press(&mut m, 'm', KeyModifiers::NONE);

        assert!(m.pending_move.is_none(), "nothing was started");
        assert!(m.status.contains("two or more"), "status: {}", m.status);
    }

    #[test]
    fn the_menu_opens_on_x_and_closes_on_the_key_it_ran() {
        let mut m = model_with_two_windows();
        press(&mut m, 'x', KeyModifiers::NONE);
        assert!(m.menu, "x opens the menu");

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.menu, "Esc closes it");
        assert!(!m.quit, "without leaving the app");
    }

    #[test]
    fn a_letter_that_is_not_in_the_menu_just_closes_it() {
        // Not an error: `x` then a mistyped letter is a slip, and a complaint
        // about it would be noise on a screen that was showing the answer.
        let mut m = model_with_two_windows();
        press(&mut m, 'x', KeyModifiers::NONE);
        press(&mut m, 'z', KeyModifiers::NONE);

        assert!(!m.menu);
        assert!(m.status.is_empty(), "status: {}", m.status);
        assert!(!m.quit);
    }

    #[test]
    fn a_disabled_row_says_what_is_missing_when_pressed() {
        // The menu shows the reason greyed out; pressing it must not silently
        // do nothing, which reads as a broken key.
        let mut m = model_with_two_windows();
        press(&mut m, 'x', KeyModifiers::NONE);
        press(&mut m, 'b', KeyModifiers::NONE); // break, with no pane selected

        assert!(!m.menu);
        assert!(m.status.contains("needs a pane"), "status: {}", m.status);
    }

    #[test]
    fn the_menu_shields_the_tree_keys() {
        // `j` moves the cursor in the tree and joins inside the menu. Whichever
        // it means, it must not do both.
        let mut m = model_with_two_windows();
        let before = m.cursor;
        press(&mut m, 'x', KeyModifiers::NONE);
        press(&mut m, 'j', KeyModifiers::NONE);

        assert_eq!(m.cursor, before, "j did not also move the cursor");
    }

    #[test]
    fn the_prefix_twice_is_the_close_it_used_to_be() {
        // `x` alone killed a window before this menu existed. Two taps still
        // reach it, which keeps the muscle memory and is strictly safer than
        // the single press it replaces.
        let mut m = model_with_two_windows();
        press(&mut m, 'x', KeyModifiers::NONE);

        assert_eq!(
            actions(&m)
                .into_iter()
                .find(|a| a.kind == ActionKind::Kill)
                .map(|a| a.enabled),
            Some(true),
            "a live window is selected, so close is on offer",
        );
    }

    #[test]
    fn slash_filters_the_destinations_instead_of_cancelling() {
        let mut m = model_with_two_windows();
        assert!(m.begin_move(vec![("%11".into(), "projects:1".into())], MoveKind::Panes));

        press(&mut m, '/', KeyModifiers::NONE);
        assert!(m.searching, "typing now narrows the choices");
        assert!(m.pending_move.is_some(), "the move is still pending");

        // `x` kills a window in the tree. Inside the filter it is a letter.
        press(&mut m, 'x', KeyModifiers::NONE);
        assert_eq!(m.search, "x");
        assert!(m.pending_move.is_some());

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.searching, "Esc closes the filter");
        assert!(
            m.pending_move.is_some(),
            "and only the filter — the move survives one more Esc",
        );
    }

    #[test]
    fn cancelling_a_move_takes_its_filter_with_it() {
        // The query was typed to find a destination; leaving it applied to
        // the tree would hide most of the workspace with no obvious cause.
        let mut m = model_with_two_windows();
        assert!(m.begin_move(vec![("%11".into(), "projects:1".into())], MoveKind::Panes));
        m.searching = true;
        m.search_push('b');

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);

        assert!(m.pending_move.is_none());
        assert!(m.search.is_empty(), "search: {:?}", m.search);
        assert!(!m.quit, "cancelling is not leaving");
    }

    #[test]
    fn a_window_command_prefers_the_marked_windows_over_the_cursor() {
        let mut m = model_with_two_windows();
        m.cursor = 0; // projects:1
        m.marks.insert("projects:2".into());

        assert_eq!(
            selected_windows(&m),
            vec!["projects:2".to_string()],
            "a selection is an explicit answer; the cursor is only a fallback",
        );

        m.clear_marks();
        assert_eq!(selected_windows(&m), vec!["projects:1".to_string()]);
    }

    #[test]
    fn a_pane_command_leaves_the_restore_selection_alone() {
        // Windows staged for `r` and panes staged for `J` coexist in one set.
        // A move that cleared everything would quietly undo the staging.
        let mut m = model_with_two_windows();
        m.marks.insert("projects:1".into());
        m.marks.insert("%11".into());

        m.unmark(&["%11".to_string()]);

        assert_eq!(
            m.marks.iter().cloned().collect::<Vec<_>>(),
            vec!["projects:1".to_string()],
        );
    }

    #[test]
    fn ctrl_c_leaves_from_the_search_line_too() {
        let mut m = model_with_window();
        m.searching = true;
        m.search_push('a');
        let _ = handle_key(&mut m, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(m.quit, "ctrl-c is an unconditional way out");
    }

    #[test]
    fn ctrl_c_quits_rather_than_clearing_marks() {
        // Both are bound to `c`; the modifier guard has to win.
        let mut m = model_with_window();
        m.marks.insert("projects:1".into());

        press(&mut m, 'c', KeyModifiers::CONTROL);
        assert!(m.quit, "ctrl-c must quit");
        assert_eq!(m.marks.len(), 1, "and must not clear marks on the way out");
    }

    #[test]
    fn plain_c_clears_marks_without_quitting() {
        let mut m = model_with_window();
        m.marks.insert("projects:1".into());

        press(&mut m, 'c', KeyModifiers::NONE);
        assert!(m.marks.is_empty());
        assert!(!m.quit);
    }

    #[test]
    fn enter_on_a_window_that_only_exists_in_the_point_explains_itself() {
        let mut m = model_with_window();
        if let Row::Window(w) = &mut m.rows[0] {
            w.gone = true;
        }
        let _ = handle_key(&mut m, KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(m.switch_to, None, "there is nothing to switch to");
        assert!(m.status.contains("restore"), "status: {}", m.status);
    }

    #[test]
    fn enter_on_a_live_window_records_the_switch() {
        let mut m = model_with_window();
        let _ = handle_key(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.switch_to.as_deref(), Some("projects:1"));
    }

    #[test]
    fn a_new_keypress_clears_the_previous_status() {
        // Otherwise a stale message sits under an unrelated action.
        let mut m = model_with_window();
        m.status.info("something happened");
        press(&mut m, 'j', KeyModifiers::NONE);
        assert!(m.status.is_empty());
    }
    // --- CJK input source ---------------------------------------------------

    // Under a Korean input source every shortcut arrives as a jamo. tmc starts
    // in search mode, so the first thing a user does is Tab out to the tree --
    // where, without this mapping, the whole keyboard was dead.
    #[test]
    fn hangul_clears_marks_like_latin_c() {
        let mut m = model_with_window();
        m.searching = false;
        m.marks.insert("projects:1".into());

        // `ㅊ` is the physical `c` under the 2-set layout.
        press(&mut m, 'ㅊ', KeyModifiers::NONE);
        assert!(m.marks.is_empty(), "ㅊ (physical c) must clear marks");
        assert!(!m.quit);
    }

    #[test]
    fn hangul_quits_like_latin_q() {
        let mut m = model_with_window();
        m.searching = false;
        // `ㅂ` sits on the physical `q` key.
        press(&mut m, 'ㅂ', KeyModifiers::NONE);
        assert!(m.quit, "ㅂ (physical q) must quit");
    }

    // Search mode is where letters type, so a jamo there is the query -- a
    // Korean window name would otherwise be unsearchable.
    #[test]
    fn search_mode_keeps_hangul_verbatim() {
        let mut m = model_with_window();
        m.searching = true;
        press(&mut m, 'ㅂ', KeyModifiers::NONE);
        assert_eq!(m.search, "ㅂ", "searching must not rewrite the jamo");
        assert!(!m.quit);
    }

    fn pending_move() -> crate::ui::model::PendingMove {
        crate::ui::model::PendingMove {
            panes: vec![("%1084".into(), "projects:2".into())],
            kind: MoveKind::Panes,
            destination: Some("tooling:3".into()),
        }
    }

    // The pane-move picker is its own mode ahead of the tree keys, and it is
    // all shortcuts, so it normalizes too.
    #[test]
    fn hangul_moves_the_destination_cursor_in_pane_move() {
        let mut with_jamo = model_with_window();
        with_jamo.searching = false;
        with_jamo.pending_move = Some(pending_move());
        // `ㅓ` is the physical `j`.
        press(&mut with_jamo, 'ㅓ', KeyModifiers::NONE);

        let mut with_latin = model_with_window();
        with_latin.searching = false;
        with_latin.pending_move = Some(pending_move());
        press(&mut with_latin, 'j', KeyModifiers::NONE);

        // j is a bound navigation key, so it leaves no "choose a window" hint;
        // an unmapped key would. Asserting both are empty AND equal makes the
        // comparison meaningful rather than two identical error strings.
        assert_eq!(with_latin.status, "", "j should navigate, not fall through");
        assert_eq!(
            with_jamo.status, with_latin.status,
            "ㅓ must reach the same handler j does"
        );
    }

    // --- off the input thread ----------------------------------------------

    fn reloaded(generation: u64, window: u32) -> Reply {
        Reply::Reloaded {
            generation,
            snapshot: Ok(Snapshot::for_test("projects", window)),
        }
    }

    fn has_window(m: &Model, target: &str) -> bool {
        m.rows
            .iter()
            .any(|r| matches!(r, Row::Window(w) if w.target() == target))
    }

    #[test]
    fn a_superseded_reload_is_dropped() {
        // A slow tick that started before `p` must not paint the old point's
        // reading over the new one.
        let mut io = Io::new(None);
        let mut m = Model::new(Vec::new());
        io.generation = 2;
        io.reloads = 2;

        assert!(!io.apply(&mut m, reloaded(1, 1)), "nothing drawn");
        assert!(m.rows.is_empty(), "the stale reading was not applied");
        assert_eq!(io.reloads, 1, "but it no longer counts as outstanding");

        assert!(io.apply(&mut m, reloaded(2, 5)), "the current one redraws");
        assert!(has_window(&m, "projects:5"));
        assert_eq!(io.reloads, 0, "so the tick may start the next one");
    }

    #[test]
    fn an_unchanged_reload_does_not_redraw() {
        // The fingerprint gate: an idle workspace stays at 0 fps.
        let mut io = Io::new(None);
        let mut m = Model::new(Vec::new());
        io.generation = 1;
        assert!(io.apply(&mut m, reloaded(1, 1)));
        io.generation = 2;
        assert!(!io.apply(&mut m, reloaded(2, 1)), "same tree, no frame");
    }

    #[test]
    fn a_reload_waits_while_the_menu_is_up() {
        let mut io = Io::new(None);
        let mut m = Model::new(Vec::new());
        m.menu = true;
        io.generation = 1;
        assert!(!io.apply(&mut m, reloaded(1, 1)));
        assert!(m.rows.is_empty(), "the menu's target did not move under it");
    }

    #[test]
    fn a_failed_reply_keeps_the_tree_and_the_popup() {
        let mut io = Io::new(None);
        let mut m = Model::new(Vec::new());
        io.generation = 1;
        io.apply(&mut m, reloaded(1, 1));
        io.generation = 2;
        io.apply(
            &mut m,
            Reply::Reloaded {
                generation: 2,
                snapshot: Err(anyhow::anyhow!("ps timed out")),
            },
        );
        assert!(has_window(&m, "projects:1"), "last good tree stays");
        assert!(m.status.contains("ps timed out"), "{}", m.status);
        assert!(!m.quit);
    }

    #[test]
    fn a_finished_command_reports_after_the_tree_is_re_read() {
        let mut io = Io::new(None);
        let mut m = Model::new(Vec::new());
        io.run(
            &mut m,
            Effect::Job(Job::new("closing", || {
                Ok(Done::status(crate::ui::status::Level::Info, "unused"))
            })),
        );
        assert_eq!(m.busy, Some("closing"));
        let generation = io.generation;
        // Drain the real worker's reply so its thread is not left sending
        // into a dropped channel; then feed a controlled one.
        let _ = io.workers.recv();

        let seen = std::sync::Arc::new(std::sync::Mutex::new(false));
        let witness = seen.clone();
        let changed = io.apply(
            &mut m,
            Reply::JobDone {
                generation,
                done: Ok(Done::then(move |m: &mut Model| {
                    *witness.lock().unwrap() = has_window(m, "projects:3");
                    m.status.success("killed 1 window(s)");
                })),
                snapshot: Ok(Snapshot::for_test("projects", 3)),
            },
        );
        assert!(changed);
        assert!(*seen.lock().unwrap(), "finish saw the re-read tree");
        assert_eq!(m.busy, None);
        assert_eq!(m.status, "killed 1 window(s)");
    }

    #[test]
    fn keys_stay_live_while_a_command_runs() {
        let mut m = model_with_two_windows();
        m.busy = Some("restoring");

        press(&mut m, 'j', KeyModifiers::NONE);
        assert_eq!(m.cursor, 1, "navigation is not blocked");

        assert!(matches!(
            handle_key(&mut m, KeyCode::Char('s'), KeyModifiers::NONE),
            Effect::None
        ));
        assert!(m.status.contains("still restoring"), "{}", m.status);

        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(m.quit, "Esc answers; the loop then waits for the restore");
    }

    #[test]
    fn expanding_rebuilds_from_the_last_reading_without_shelling_out() {
        let mut m = model_with_window();
        assert!(matches!(
            handle_key(&mut m, KeyCode::Char('l'), KeyModifiers::NONE),
            Effect::Rebuild
        ));
        assert!(m.expanded.contains("projects:1"));
    }

    // --- one table ----------------------------------------------------------

    #[test]
    fn question_mark_opens_help_and_esc_closes_it_without_quitting() {
        let mut m = model_with_window();
        press(&mut m, '?', KeyModifiers::NONE);
        assert!(m.help);
        // `j` means nothing in help; it must not move the tree behind it.
        press(&mut m, 'j', KeyModifiers::NONE);
        assert_eq!(m.cursor, 0);
        let _ = handle_key(&mut m, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!m.help);
        assert!(!m.quit);
    }

    #[test]
    fn question_mark_types_on_the_search_line() {
        let mut m = model_with_window();
        m.searching = true;
        press(&mut m, '?', KeyModifiers::NONE);
        assert_eq!(m.search, "?");
        assert!(!m.help);
    }

    #[test]
    fn ctrl_u_drops_a_restored_query() {
        let mut m = model_with_window();
        m.searching = true;
        m.search = "old query".into();
        press(&mut m, 'u', KeyModifiers::CONTROL);
        assert!(m.search.is_empty());
        assert!(m.searching, "still on the search line");
    }

    #[test]
    fn every_tree_binding_dispatches_to_its_command() {
        // The hint row and help are read off the table; dispatch must agree
        // with them key for key. Checked by effect for the keys whose effect
        // is visible on a bare model.
        for b in keys::BINDINGS.iter().filter(|b| b.ctx == Ctx::Tree) {
            assert_eq!(
                keys::lookup(Ctx::Tree, b.key).map(|x| x.cmd),
                Some(b.cmd),
                "{:?}",
                b.key
            );
        }
        let mut m = model_with_window();
        press(&mut m, 'x', KeyModifiers::NONE);
        assert!(m.menu, "x is the menu, as the hint says");
    }

    // --- a restored query ---------------------------------------------------

    fn restored(query: &str) -> Model {
        let mut m = model_with_window();
        crate::ui::state::UiState {
            search: query.into(),
            ..Default::default()
        }
        .restore(&mut m);
        m
    }

    #[test]
    fn typing_after_a_restored_query_replaces_it() {
        let mut m = restored("foo");
        press(&mut m, '/', KeyModifiers::NONE);
        press(&mut m, 'b', KeyModifiers::NONE);
        assert_eq!(m.search, "b");
        press(&mut m, 'a', KeyModifiers::NONE);
        assert_eq!(m.search, "ba", "only the first key replaces");
    }

    #[test]
    fn enter_accepts_a_restored_query_as_it_is() {
        let mut m = restored("foo");
        press(&mut m, '/', KeyModifiers::NONE);
        let _ = handle_key(&mut m, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.search, "foo");
        assert!(!m.search_selected);
    }

    #[test]
    fn a_move_accepts_a_restored_query_and_later_typing_appends() {
        let mut m = restored("foo");
        m.searching = true; // the popup opens on the search line
        let _ = handle_key(&mut m, KeyCode::Down, KeyModifiers::NONE);
        press(&mut m, 'd', KeyModifiers::NONE);
        assert_eq!(m.search, "food");
    }

    #[test]
    fn backspace_clears_a_restored_query_whole() {
        let mut m = restored("foo");
        m.searching = true;
        let _ = handle_key(&mut m, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(m.search, "");
        assert!(m.searching);
    }

    #[test]
    fn a_restored_query_still_filters_the_tree_until_searched() {
        // Out on the tree, tree keys keep their meaning and the selection
        // waits for the search line.
        let mut m = restored("foo");
        press(&mut m, 'j', KeyModifiers::NONE);
        assert!(m.search_selected);
        assert_eq!(m.search, "foo");
    }
}
