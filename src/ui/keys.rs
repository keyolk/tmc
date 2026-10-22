//! Every key the TUI answers to, in one table.
//!
//! The table is what `app::handle_key` dispatches on, what the hint row
//! prints, what the `x` menu lists and what `?` help shows. They used to be
//! four hand-written things — a `match` per mode, a hint string per mode, the
//! menu rows and nothing at all for help — and a key added to one had to be
//! remembered in the others. Now a row here is the only place a key exists.

use crossterm::event::{KeyCode, KeyModifiers};
use unicode_width::UnicodeWidthStr;

use super::model::{ActionKind, Model};
use super::split::Action as SplitAction;

/// A key as the table names it: modifiers folded in, Shift spelled by case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Esc,
    Enter,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
}

impl Key {
    /// The table's name for a terminal key event; `None` for keys nothing binds.
    pub fn from_event(code: KeyCode, mods: KeyModifiers) -> Option<Key> {
        Some(match code {
            KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
                Key::Ctrl(c.to_ascii_lowercase())
            }
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Esc => Key::Esc,
            KeyCode::Enter => Key::Enter,
            KeyCode::Tab => Key::Tab,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            _ => return None,
        })
    }

    /// How the key is written in hints and help. `glyphs` is the theme's
    /// `key_glyphs`: arrows and symbols where they are safe, words otherwise.
    pub fn label(self, glyphs: bool) -> String {
        if !glyphs {
            let word = match self {
                Key::Enter => Some("enter"),
                Key::Backspace => Some("bksp"),
                Key::Up => Some("up"),
                Key::Down => Some("down"),
                Key::Left => Some("left"),
                Key::Right => Some("right"),
                _ => None,
            };
            if let Some(word) = word {
                return word.into();
            }
        }
        match self {
            Key::Char(' ') => "space".into(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("ctrl-{c}"),
            Key::Esc => "esc".into(),
            Key::Enter => "⏎".into(),
            Key::Tab => "tab".into(),
            Key::Backspace => "⌫".into(),
            Key::Up => "↑".into(),
            Key::Down => "↓".into(),
            Key::Left => "←".into(),
            Key::Right => "→".into(),
            Key::Home => "home".into(),
            Key::End => "end".into(),
        }
    }
}

/// Which keys are live. Innermost wins; `Global` answers in every context
/// after the context's own rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctx {
    Global,
    /// The search line: letters type.
    Search,
    /// The tree: letters are commands.
    Tree,
    /// The `x` menu, waiting for its second key.
    Menu,
    /// Choosing the window a pane move lands in.
    Move,
    /// Typing a filter while choosing that window: letters type.
    MoveSearch,
    /// Typing the name of a new session: letters type.
    Name,
    /// The `?` overlay.
    Help,
}

impl Ctx {
    /// Whether unbound printable keys are input rather than ignored.
    pub fn types(self) -> bool {
        matches!(self, Ctx::Search | Ctx::MoveSearch | Ctx::Name)
    }

    fn title(self) -> &'static str {
        match self {
            Ctx::Global => "anywhere",
            Ctx::Search => "search line",
            Ctx::Tree => "tree",
            Ctx::Menu => "x menu",
            Ctx::Move => "choosing a destination",
            Ctx::MoveSearch => "filtering destinations",
            Ctx::Name => "naming a new session",
            Ctx::Help => "this help",
        }
    }
}

/// What a key does. `app::run_cmd` matches on this exhaustively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Quit,
    Search,
    ToTree,
    SearchPop,
    SearchClear,
    Down,
    Up,
    Top,
    Bottom,
    Switch,
    Mark,
    MarkAll,
    ClearMarks,
    NextWaiting,
    PointNext,
    PointPrev,
    Save,
    Restore,
    Expand,
    Collapse,
    Menu,
    CloseMenu,
    Run(ActionKind),
    Reload,
    OpenLink,
    Help,
    CloseHelp,
    HelpDown,
    HelpUp,
    DestDown,
    DestUp,
    Confirm,
    CancelMove,
    FilterOff,
    /// Move the tree | detail divider.
    Split(SplitAction),
    NamePop,
    CreateSession,
    CancelName,
}

/// Where a binding sits in the hint row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    /// Listed in `?` help only.
    None,
    /// Kept together with the rest of its group; higher groups are dropped
    /// first when the row is too narrow.
    Group(u8),
    /// Always shown, at the end: the way out must never be cut.
    Pinned,
}

pub struct Binding {
    pub ctx: Ctx,
    pub key: Key,
    pub cmd: Cmd,
    /// The word in the hint row; consecutive rows of one group with the same
    /// word share it (`j/k move`).
    pub hint_word: &'static str,
    pub hint: Hint,
    /// One line for `?` help, and the `x` menu's label for a `Run` row.
    pub help: &'static str,
}

const fn b(
    ctx: Ctx,
    key: Key,
    cmd: Cmd,
    hint: Hint,
    hint_word: &'static str,
    help: &'static str,
) -> Binding {
    Binding {
        ctx,
        key,
        cmd,
        hint_word,
        hint,
        help,
    }
}

use Hint::{Group, None as Hidden, Pinned};
use Key::Char as C;

/// The table. Order matters twice: it is the hint row's order within a
/// group, and it is the `x` menu's row order.
#[rustfmt::skip]
pub static BINDINGS: &[Binding] = &[
    // Ctrl-c is an unconditional way out, including from the text inputs.
    b(Ctx::Global, Key::Ctrl('c'), Cmd::Quit, Pinned, "quit", "quit"),
    // --- search line: where the TUI starts, so it names the way out and the
    // way to the other commands; otherwise they are undiscoverable from here.
    b(Ctx::Search, Key::Up, Cmd::Up, Group(1), "move", "previous match"),
    b(Ctx::Search, Key::Down, Cmd::Down, Group(1), "move", "next match"),
    b(Ctx::Search, Key::Ctrl('p'), Cmd::Up, Hidden, "", "previous match"),
    b(Ctx::Search, Key::Ctrl('n'), Cmd::Down, Hidden, "", "next match"),
    b(Ctx::Search, Key::Enter, Cmd::Switch, Group(1), "switch", "switch to the window"),
    // Esc is a mode change here, never an exit; both keep the query as a
    // visible filter so backing out does not discard typing.
    b(Ctx::Search, Key::Tab, Cmd::ToTree, Group(1), "commands", "to the tree, keeping the filter"),
    b(Ctx::Search, Key::Esc, Cmd::ToTree, Group(1), "commands", "to the tree, keeping the filter"),
    b(Ctx::Search, Key::Backspace, Cmd::SearchPop, Hidden, "", "delete a character"),
    b(Ctx::Search, Key::Ctrl('u'), Cmd::SearchClear, Group(2), "clear", "clear the query"),
    // --- tree. Ordered by how often each is reached for.
    b(Ctx::Tree, C('/'), Cmd::Search, Group(1), "search", "search windows and panes"),
    b(Ctx::Tree, C('j'), Cmd::Down, Group(1), "move", "down"),
    b(Ctx::Tree, C('k'), Cmd::Up, Group(1), "move", "up"),
    b(Ctx::Tree, Key::Down, Cmd::Down, Hidden, "", "down"),
    b(Ctx::Tree, Key::Up, Cmd::Up, Hidden, "", "up"),
    b(Ctx::Tree, Key::Enter, Cmd::Switch, Group(1), "switch", "switch to the window"),
    // The menu sits in the first group: it is the way to every command that
    // rearranges tmux, and the menu behind it is self-describing, so one
    // short word is enough. At 80 columns this group and `? help` are what
    // survive; the help names everything the row had to drop.
    b(Ctx::Tree, C('x'), Cmd::Menu, Group(1), "menu", "session, window and pane commands"),
    b(Ctx::Tree, C(' '), Cmd::Mark, Group(2), "mark", "mark the session, window or pane"),
    b(Ctx::Tree, C('a'), Cmd::MarkAll, Group(2), "all", "mark every changed window"),
    b(Ctx::Tree, C('r'), Cmd::Restore, Group(2), "restore", "restore the marked sessions and windows, or every missing one"),
    // Expanding comes before the rarer commands: it is how you reach a pane
    // at all, and a hint that drops it first leaves the pane commands looking
    // like they act on nothing.
    b(Ctx::Tree, C('l'), Cmd::Expand, Group(3), "panes", "list the window's panes"),
    b(Ctx::Tree, C('h'), Cmd::Collapse, Group(3), "panes", "hide the window's panes"),
    b(Ctx::Tree, Key::Right, Cmd::Expand, Hidden, "", "list the window's panes"),
    b(Ctx::Tree, Key::Left, Cmd::Collapse, Hidden, "", "hide the window's panes"),
    b(Ctx::Tree, C('s'), Cmd::Save, Group(4), "save", "save a restore point"),
    b(Ctx::Tree, C('n'), Cmd::NextWaiting, Group(4), "waiting", "next window waiting for you"),
    // `o` is the fleet's open-link key (tui-design E11).
    b(Ctx::Tree, C('o'), Cmd::OpenLink, Group(5), "open link", "open the first link in the output on screen"),
    b(Ctx::Tree, C('p'), Cmd::PointNext, Group(6), "point", "older restore point"),
    b(Ctx::Tree, C('P'), Cmd::PointPrev, Group(6), "point", "newer restore point"),
    b(Ctx::Tree, C('c'), Cmd::ClearMarks, Hidden, "", "clear the marks"),
    b(Ctx::Tree, C('g'), Cmd::Top, Hidden, "", "first row"),
    b(Ctx::Tree, Key::Home, Cmd::Top, Hidden, "", "first row"),
    b(Ctx::Tree, C('G'), Cmd::Bottom, Hidden, "", "last row"),
    b(Ctx::Tree, Key::End, Cmd::Bottom, Hidden, "", "last row"),
    b(Ctx::Tree, C('R'), Cmd::Reload, Hidden, "", "re-read tmux now"),
    // The split, last: resizing is rare next to everything above. Shrink
    // before grow so the hint reads `</>/=` in the order of the keyboard.
    b(Ctx::Tree, C('<'), Cmd::Split(SplitAction::Shrink), Group(7), "split", "narrow the tree"),
    b(Ctx::Tree, C('>'), Cmd::Split(SplitAction::Grow), Group(7), "split", "widen the tree"),
    b(Ctx::Tree, C('='), Cmd::Split(SplitAction::Reset), Group(7), "split", "reset the tree | detail split"),
    b(Ctx::Tree, C('?'), Cmd::Help, Pinned, "help", "this help"),
    // Esc pops one level: the pane-move choice, then the search line, then
    // the app. Doing nothing at the root would make the key that got you out
    // of everything else stop working exactly once.
    b(Ctx::Tree, Key::Esc, Cmd::Quit, Pinned, "quit", "quit"),
    b(Ctx::Tree, C('q'), Cmd::Quit, Pinned, "quit", "quit"),
    // --- the `x` menu. A `Run` row's help is its menu label.
    b(Ctx::Menu, C('b'), Cmd::Run(ActionKind::Break), Hidden, "", "break out"),
    b(Ctx::Menu, C('j'), Cmd::Run(ActionKind::Join), Hidden, "", "join into a window"),
    b(Ctx::Menu, C('m'), Cmd::Run(ActionKind::Merge), Hidden, "", "merge windows"),
    b(Ctx::Menu, C('s'), Cmd::Run(ActionKind::Send), Hidden, "", "send to other session"),
    b(Ctx::Menu, C('k'), Cmd::Run(ActionKind::Kill), Hidden, "", "close"),
    // The prefix again is the old `x`, which alone used to close a window:
    // the muscle memory still works, in two keystrokes rather than one.
    b(Ctx::Menu, C('x'), Cmd::Run(ActionKind::Kill), Hidden, "", "close (x x)"),
    // Sessions after windows: rarer, and closing one takes every window in it.
    b(Ctx::Menu, C('n'), Cmd::Run(ActionKind::NewSession), Hidden, "", "new session"),
    b(Ctx::Menu, C('d'), Cmd::Run(ActionKind::KillSession), Hidden, "", "close session"),
    b(Ctx::Menu, Key::Esc, Cmd::CloseMenu, Pinned, "cancel", "close the menu"),
    // --- choosing where a pane move lands.
    b(Ctx::Move, C('j'), Cmd::DestDown, Group(1), "window", "next window"),
    b(Ctx::Move, C('k'), Cmd::DestUp, Group(1), "window", "previous window"),
    b(Ctx::Move, Key::Down, Cmd::DestDown, Hidden, "", "next window"),
    b(Ctx::Move, Key::Up, Cmd::DestUp, Hidden, "", "previous window"),
    b(Ctx::Move, C('/'), Cmd::Search, Group(1), "filter", "filter the windows"),
    b(Ctx::Move, Key::Enter, Cmd::Confirm, Group(1), "confirm", "move into this window"),
    b(Ctx::Move, C('q'), Cmd::Quit, Hidden, "", "quit"),
    b(Ctx::Move, Key::Esc, Cmd::CancelMove, Pinned, "cancel", "cancel the move"),
    // --- typing a filter while choosing.
    b(Ctx::MoveSearch, Key::Enter, Cmd::Confirm, Group(1), "confirm", "move into this window"),
    b(Ctx::MoveSearch, Key::Down, Cmd::DestDown, Hidden, "", "next window"),
    b(Ctx::MoveSearch, Key::Up, Cmd::DestUp, Hidden, "", "previous window"),
    b(Ctx::MoveSearch, Key::Ctrl('n'), Cmd::DestDown, Hidden, "", "next window"),
    b(Ctx::MoveSearch, Key::Ctrl('p'), Cmd::DestUp, Hidden, "", "previous window"),
    b(Ctx::MoveSearch, Key::Backspace, Cmd::SearchPop, Hidden, "", "delete a character"),
    b(Ctx::MoveSearch, Key::Tab, Cmd::FilterOff, Hidden, "", "stop filtering"),
    b(Ctx::MoveSearch, Key::Esc, Cmd::FilterOff, Pinned, "filter off", "stop filtering"),
    // --- naming a new session.
    b(Ctx::Name, Key::Enter, Cmd::CreateSession, Group(1), "create", "create the session"),
    b(Ctx::Name, Key::Backspace, Cmd::NamePop, Hidden, "", "delete a character"),
    b(Ctx::Name, Key::Esc, Cmd::CancelName, Pinned, "cancel", "cancel"),
    // --- help.
    b(Ctx::Help, C('?'), Cmd::CloseHelp, Hidden, "", "close"),
    b(Ctx::Help, C('q'), Cmd::CloseHelp, Hidden, "", "close"),
    // Help is longer than a popup is tall.
    b(Ctx::Help, C('j'), Cmd::HelpDown, Group(1), "scroll", "scroll down"),
    b(Ctx::Help, C('k'), Cmd::HelpUp, Group(1), "scroll", "scroll up"),
    b(Ctx::Help, Key::Down, Cmd::HelpDown, Hidden, "", "scroll down"),
    b(Ctx::Help, Key::Up, Cmd::HelpUp, Hidden, "", "scroll up"),
    b(Ctx::Help, Key::Esc, Cmd::CloseHelp, Pinned, "close", "close"),
];

/// The context the model is in, which decides what a key means.
pub fn context(model: &Model) -> Ctx {
    if model.help {
        Ctx::Help
    } else if model.naming.is_some() {
        Ctx::Name
    } else if model.pending_move.is_some() {
        if model.searching {
            Ctx::MoveSearch
        } else {
            Ctx::Move
        }
    } else if model.menu {
        Ctx::Menu
    } else if model.searching {
        Ctx::Search
    } else {
        Ctx::Tree
    }
}

/// The binding a key reaches in `ctx`: the context's own first, then global.
pub fn lookup(ctx: Ctx, key: Key) -> Option<&'static Binding> {
    let find = |c: Ctx| BINDINGS.iter().find(|b| b.ctx == c && b.key == key);
    find(ctx).or_else(|| find(Ctx::Global))
}

fn rows(ctx: Ctx) -> impl Iterator<Item = &'static Binding> {
    BINDINGS.iter().filter(move |b| b.ctx == ctx)
}

/// The word a hint shows, which for `r` depends on the selection: restoring
/// with nothing marked restores every missing window, and the hint should
/// say which of the two a press will do.
fn word(b: &Binding, model: &Model) -> String {
    match b.cmd {
        Cmd::Restore if model.marks.is_empty() => "restore all".into(),
        Cmd::Restore => "restore marked".into(),
        _ => b.hint_word.into(),
    }
}

/// Bindings to hint entries: `keys word`, merging neighbours that share a
/// word so `j` and `k` read as `j/k move`.
fn entries<'a>(list: impl Iterator<Item = &'a Binding>, model: &Model) -> Vec<String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for b in list {
        let w = word(b, model);
        match out.last_mut() {
            Some((keys, last)) if *last == w => {
                keys.push('/');
                keys.push_str(&b.key.label(model.theme.glyphs.key_glyphs));
            }
            _ => out.push((b.key.label(model.theme.glyphs.key_glyphs), w)),
        }
    }
    out.into_iter().map(|(k, w)| format!("{k} {w}")).collect()
}

/// The hint row for `ctx`, fitted to `width`.
///
/// Groups are dropped whole from the rare end until the row fits: a hint cut
/// mid-word is worse than a short one. Pinned entries — the way out — are
/// always kept. A context with none of its own borrows the global ones, so
/// the search line still says how to leave.
pub fn footer(ctx: Ctx, model: &Model, width: usize) -> String {
    let mut pinned: Vec<&Binding> = rows(ctx).filter(|b| b.hint == Pinned).collect();
    if pinned.is_empty() {
        pinned = rows(Ctx::Global).filter(|b| b.hint == Pinned).collect();
    }
    let tail = entries(pinned.into_iter(), model).join("   ");

    let mut groups: Vec<u8> = rows(ctx)
        .filter_map(|b| match b.hint {
            Group(g) => Some(g),
            _ => None,
        })
        .collect();
    groups.sort_unstable();
    groups.dedup();

    let mut text = String::new();
    for g in groups {
        let group = entries(rows(ctx).filter(|b| b.hint == Group(g)), model).join("  ");
        let candidate = if text.is_empty() {
            group
        } else {
            format!("{text}   {group}")
        };
        if candidate.width() + 3 + tail.width() > width {
            break;
        }
        text = candidate;
    }
    match (text.is_empty(), tail.is_empty()) {
        (_, true) => text,
        (true, false) => tail,
        (false, false) => format!("{text}   {tail}"),
    }
}

/// `?` help: every context and what each of its keys does, keys that do the
/// same thing on one line.
pub fn help_lines(glyphs: bool) -> Vec<(String, Vec<(String, String)>)> {
    [
        Ctx::Search,
        Ctx::Tree,
        Ctx::Menu,
        Ctx::Move,
        Ctx::MoveSearch,
        Ctx::Name,
        Ctx::Global,
    ]
    .into_iter()
    .map(|ctx| {
        let mut lines: Vec<(Cmd, String, String)> = Vec::new();
        for b in rows(ctx) {
            match lines.iter_mut().find(|(cmd, _, _)| *cmd == b.cmd) {
                Some((_, keys, _)) => {
                    keys.push('/');
                    keys.push_str(&b.key.label(glyphs));
                }
                None => lines.push((b.cmd, b.key.label(glyphs), b.help.to_string())),
            }
        }
        (
            ctx.title().to_string(),
            lines.into_iter().map(|(_, k, h)| (k, h)).collect(),
        )
    })
    .collect()
}

/// The `x` menu's rows: each command once, with its first key.
pub fn menu_rows() -> Vec<(ActionKind, char, &'static str)> {
    let mut out: Vec<(ActionKind, char, &'static str)> = Vec::new();
    for b in rows(Ctx::Menu) {
        if let (Cmd::Run(kind), Key::Char(c)) = (b.cmd, b.key)
            && !out.iter().any(|(k, _, _)| *k == kind)
        {
            out.push((kind, c, b.help));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const ALL: [Ctx; 8] = [
        Ctx::Global,
        Ctx::Search,
        Ctx::Tree,
        Ctx::Menu,
        Ctx::Move,
        Ctx::MoveSearch,
        Ctx::Name,
        Ctx::Help,
    ];

    #[test]
    fn no_key_is_bound_twice_in_one_context() {
        for ctx in ALL {
            let mut seen = HashSet::new();
            for b in rows(ctx) {
                assert!(seen.insert(b.key), "{:?} is bound twice in {ctx:?}", b.key);
            }
        }
    }

    #[test]
    fn no_context_shadows_the_global_way_out() {
        for b in rows(Ctx::Global) {
            for ctx in ALL {
                assert_eq!(
                    lookup(ctx, b.key).map(|x| x.cmd),
                    Some(b.cmd),
                    "{:?} in {ctx:?} no longer does {:?}",
                    b.key,
                    b.cmd,
                );
            }
        }
    }

    #[test]
    fn every_binding_in_help_is_reachable_by_its_key() {
        // Help is derived from the table, so this is the same as saying no
        // row is dead: whatever help lists, pressing it in that context runs
        // exactly that row.
        for b in BINDINGS {
            let hit = lookup(b.ctx, b.key).expect("bound");
            assert!(
                std::ptr::eq(hit, b),
                "{:?} in {:?} is shadowed",
                b.key,
                b.ctx
            );
        }
        let listed: usize = help_lines(true).iter().map(|(_, l)| l.len()).sum();
        assert!(listed > 0);
    }

    #[test]
    fn text_contexts_bind_no_printable_letters() {
        // Letters type there; a bound one would be a letter you cannot search
        // for.
        for ctx in [Ctx::Search, Ctx::MoveSearch, Ctx::Name] {
            for b in rows(ctx) {
                assert!(!matches!(b.key, Key::Char(_)), "{:?} in {ctx:?}", b.key);
            }
        }
    }

    #[test]
    fn reserved_keys_stay_unbound() {
        for b in BINDINGS {
            if let Key::Ctrl(c) = b.key {
                assert!(
                    !matches!(c, 'z' | '\\' | 's' | 'q'),
                    "ctrl-{c} belongs to the terminal"
                );
            }
        }
    }

    #[test]
    fn the_menu_lists_each_command_once() {
        let rows = menu_rows();
        let kinds: HashSet<_> = rows.iter().map(|(k, _, _)| format!("{k:?}")).collect();
        assert_eq!(kinds.len(), rows.len());
        assert_eq!(rows.len(), 7);
    }

    #[test]
    fn neighbours_with_one_word_share_it() {
        let m = Model::new(Vec::new());
        let f = footer(Ctx::Tree, &m, 400);
        assert!(f.contains("j/k move"), "{f}");
        assert!(f.contains("p/P point"), "{f}");
        assert!(f.ends_with("? help   esc/q quit"), "{f}");
        assert!(f.contains("</>/= split"), "{f}");
    }

    #[test]
    fn the_split_keys_are_the_last_hint_group_and_in_help() {
        // Dropped first on a narrow row: the commands above matter more.
        let m = Model::new(Vec::new());
        let narrow = footer(Ctx::Tree, &m, 100);
        assert!(!narrow.contains("split"), "{narrow}");
        let tree = help_lines(true)
            .into_iter()
            .find(|(t, _)| t == "tree")
            .unwrap()
            .1;
        for help in [
            "narrow the tree",
            "widen the tree",
            "reset the tree | detail split",
        ] {
            assert!(tree.iter().any(|(_, h)| h == help), "{help} not in help");
        }
    }
}
