//! Window layouts chosen by what the panes are, not by cycling tmux's five.
//!
//! `next-layout` walks even-horizontal, even-vertical, main-*, tiled in a
//! fixed order and knows nothing of the panes: two claude sessions and a
//! browser land wherever their index puts them, so pressing it is a lottery.
//! Here each pane gets a kind (claude, web, editor, shell, or its program),
//! panes of a kind sit together, and a pane that is the only one of its kind
//! — the browser beside two sessions — can take the big cell.
//!
//! tmux 3.8 takes a JSON layout whose leaves name a `pane_index` (`"i"`), so
//! the tree built here puts each pane in its cell in one `select-layout`,
//! with no swap-pane shuffling. The ids (`"I"`) are written for reading only;
//! tmux places by index.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::collect::{cmd, command, proc};

/// One pane of the window being arranged.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub index: u32,
    pub id: String,
    pub kind: String,
    /// What the preview writes in the box: the pane title, else its command.
    pub label: String,
    pub active: bool,
}

/// The window: its size and its panes in index order.
#[derive(Clone, Debug)]
pub struct Window {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub zoomed: bool,
    pub cells: Vec<Cell>,
}

/// A layout before it has a size: splits weighted, leaves naming a cell.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Leaf(usize),
    /// `side_by_side` is tmux's `h` (children left to right); otherwise `v`.
    Split {
        side_by_side: bool,
        parts: Vec<(u32, Node)>,
    },
}

/// A leaf with its place in the window, for applying and for the preview.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

pub struct Preset {
    pub name: &'static str,
    pub about: &'static str,
    build: fn(&[Cell]) -> Node,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "spotlight",
        about: "the one-of-a-kind pane big on the right, the rest grouped left",
        build: spotlight,
    },
    Preset {
        name: "focus",
        about: "the active pane big on the left, the rest grouped right",
        build: focus,
    },
    Preset {
        name: "grid",
        about: "every pane in an even grid, kinds kept together",
        build: grid,
    },
    Preset {
        name: "columns",
        about: "a column per kind, its panes stacked",
        build: columns,
    },
    Preset {
        name: "rows",
        about: "a row per kind, its panes side by side",
        build: rows,
    },
    Preset {
        name: "side-by-side",
        about: "all panes in one row, kinds kept together",
        build: side_by_side,
    },
];

pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

impl Preset {
    pub fn tree(&self, cells: &[Cell]) -> Node {
        (self.build)(cells)
    }
}

/// The kind of a pane from its command line (`None`: only a shell).
///
/// claude runs behind wrappers (`ccproxy claude --intercept=mitm`), so a
/// token match, not the program, decides it; the same for tweb's launchers.
pub fn kind_of(command: Option<&str>, has_claude_session: bool) -> String {
    let Some(line) = command else {
        return if has_claude_session {
            "claude"
        } else {
            "shell"
        }
        .into();
    };
    let tokens: Vec<&str> = line
        .split_whitespace()
        .map(|t| t.rsplit('/').next().unwrap_or(t))
        .collect();
    let has = |w: &str| tokens.contains(&w);
    // The command line decides before the session id: a browser split off a
    // claude pane runs tweb, whatever the pane's options still say.
    if has("claude") {
        "claude".into()
    } else if tokens.iter().any(|t| t.starts_with("tweb")) {
        "web".into()
    } else if ["nvim", "vim", "vi", "hx", "emacs"].iter().any(|e| has(e)) {
        "editor".into()
    } else if has_claude_session {
        "claude".into()
    } else {
        tokens.first().copied().unwrap_or("shell").to_string()
    }
}

/// Kinds in the order they are laid out, each with its cells' positions.
///
/// The busiest kind first, so the sessions you run several of lead and a
/// stray shell trails; ties keep first appearance, so the order is stable.
pub fn groups(cells: &[Cell]) -> Vec<(String, Vec<usize>)> {
    let mut out: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, c) in cells.iter().enumerate() {
        match out.iter_mut().find(|(k, _)| *k == c.kind) {
            Some((_, v)) => v.push(i),
            None => out.push((c.kind.clone(), vec![i])),
        }
    }
    // Stable sort: equal counts stay in appearance order.
    out.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
    out
}

fn line_of(side_by_side: bool, items: Vec<Node>) -> Node {
    if items.len() == 1 {
        return items.into_iter().next().unwrap();
    }
    Node::Split {
        side_by_side,
        parts: items.into_iter().map(|n| (1, n)).collect(),
    }
}

/// Every cell of the given positions, kinds together, in one line.
fn grouped_line(cells: &[Cell], keep: &[usize], side_by_side: bool) -> Node {
    let order: Vec<Node> = groups(cells)
        .into_iter()
        .flat_map(|(_, v)| v)
        .filter(|i| keep.contains(i))
        .map(Node::Leaf)
        .collect();
    line_of(side_by_side, order)
}

/// The pane that is the only one of its kind, if one stands out: a web page
/// first, then an editor, then any other lone program. A lone shell never
/// does — it is the leftover, not the thing being looked at.
fn standout(cells: &[Cell]) -> Option<usize> {
    let lone: Vec<usize> = groups(cells)
        .into_iter()
        .filter(|(k, v)| v.len() == 1 && k != "shell")
        .map(|(_, v)| v[0])
        .collect();
    let rank = |i: &usize| match cells[*i].kind.as_str() {
        "web" => 0,
        "editor" => 1,
        "claude" => 3,
        _ => 2,
    };
    lone.into_iter().min_by_key(rank)
}

/// A big cell beside a column of the rest, grouped by kind. The column takes
/// two fifths, so the big cell is the page you read.
fn main_and_rest(cells: &[Cell], main: usize, main_first: bool) -> Node {
    let rest: Vec<usize> = (0..cells.len()).filter(|&i| i != main).collect();
    if rest.is_empty() {
        return Node::Leaf(main);
    }
    let column = Node::Split {
        side_by_side: false,
        parts: groups(cells)
            .into_iter()
            .flat_map(|(_, v)| v)
            .filter(|i| rest.contains(i))
            .map(|i| (1, Node::Leaf(i)))
            .collect(),
    };
    let column = match column {
        Node::Split { parts, .. } if parts.len() == 1 => parts.into_iter().next().unwrap().1,
        n => n,
    };
    let parts = if main_first {
        vec![(3, Node::Leaf(main)), (2, column)]
    } else {
        vec![(2, column), (3, Node::Leaf(main))]
    };
    Node::Split {
        side_by_side: true,
        parts,
    }
}

fn active(cells: &[Cell]) -> usize {
    cells.iter().position(|c| c.active).unwrap_or(0)
}

fn spotlight(cells: &[Cell]) -> Node {
    // With nothing one of a kind, the active pane is what you are looking at.
    main_and_rest(
        cells,
        standout(cells).unwrap_or_else(|| active(cells)),
        false,
    )
}

fn focus(cells: &[Cell]) -> Node {
    main_and_rest(cells, active(cells), true)
}

fn grid(cells: &[Cell]) -> Node {
    let order: Vec<usize> = groups(cells).into_iter().flat_map(|(_, v)| v).collect();
    let n = order.len().max(1);
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows: Vec<Node> = order
        .chunks(cols)
        .map(|row| line_of(true, row.iter().map(|&i| Node::Leaf(i)).collect()))
        .collect();
    line_of(false, rows)
}

fn columns(cells: &[Cell]) -> Node {
    let cols = groups(cells)
        .into_iter()
        .map(|(_, v)| line_of(false, v.into_iter().map(Node::Leaf).collect()))
        .collect();
    line_of(true, cols)
}

fn rows(cells: &[Cell]) -> Node {
    let rows = groups(cells)
        .into_iter()
        .map(|(_, v)| line_of(true, v.into_iter().map(Node::Leaf).collect()))
        .collect();
    line_of(false, rows)
}

fn side_by_side(cells: &[Cell]) -> Node {
    grouped_line(cells, &(0..cells.len()).collect::<Vec<_>>(), true)
}

/// Sizes along one axis: `total` cells less a one-cell border between
/// parts, shared by weight; the last part takes the rounding.
fn share(total: u32, weights: &[u32]) -> Option<Vec<u32>> {
    let borders = weights.len().saturating_sub(1) as u32;
    let room = total.checked_sub(borders)?;
    let sum: u32 = weights.iter().sum();
    let mut out = Vec::with_capacity(weights.len());
    let mut used = 0;
    for (i, w) in weights.iter().enumerate() {
        let size = if i + 1 == weights.len() {
            room - used
        } else {
            room * w / sum
        };
        if size == 0 {
            return None;
        }
        out.push(size);
        used += size;
    }
    Some(out)
}

/// Where every leaf lands in a `w`×`h` window, in tree order, or an error
/// when some cell would have no room.
pub fn place(node: &Node, w: u32, h: u32) -> Result<Vec<(usize, Rect)>> {
    let mut out = Vec::new();
    place_into(node, Rect { x: 0, y: 0, w, h }, &mut out)?;
    Ok(out)
}

fn place_into(node: &Node, r: Rect, out: &mut Vec<(usize, Rect)>) -> Result<()> {
    match node {
        Node::Leaf(i) => out.push((*i, r)),
        Node::Split {
            side_by_side,
            parts,
        } => {
            let weights: Vec<u32> = parts.iter().map(|(w, _)| *w).collect();
            let along = if *side_by_side { r.w } else { r.h };
            let Some(sizes) = share(along, &weights) else {
                bail!("the window is too small for this layout");
            };
            let mut at = if *side_by_side { r.x } else { r.y };
            for ((_, child), size) in parts.iter().zip(sizes) {
                let sub = if *side_by_side {
                    Rect {
                        x: at,
                        y: r.y,
                        w: size,
                        h: r.h,
                    }
                } else {
                    Rect {
                        x: r.x,
                        y: at,
                        w: r.w,
                        h: size,
                    }
                };
                place_into(child, sub, out)?;
                at += size + 1;
            }
        }
    }
    Ok(())
}

/// The tmux 3.8 JSON layout for `node` over `win`.
pub fn to_json(node: &Node, win: &Window) -> Result<String> {
    place(node, win.width, win.height)?;
    let root = json_node(
        node,
        Rect {
            x: 0,
            y: 0,
            w: win.width,
            h: win.height,
        },
        &win.cells,
    );
    Ok(json!({ "V": 2, "L": root }).to_string())
}

fn json_node(node: &Node, r: Rect, cells: &[Cell]) -> Value {
    match node {
        Node::Leaf(i) => json!({
            "t": "p", "w": r.w, "h": r.h, "x": r.x, "y": r.y,
            "i": cells[*i].index, "I": cells[*i].id,
        }),
        Node::Split {
            side_by_side,
            parts,
        } => {
            let weights: Vec<u32> = parts.iter().map(|(w, _)| *w).collect();
            let along = if *side_by_side { r.w } else { r.h };
            // `to_json` placed the whole tree first, so the sizes exist.
            let sizes = share(along, &weights).unwrap_or_default();
            let mut at = if *side_by_side { r.x } else { r.y };
            let mut children = Vec::new();
            for ((_, child), size) in parts.iter().zip(sizes) {
                let sub = if *side_by_side {
                    Rect {
                        x: at,
                        y: r.y,
                        w: size,
                        h: r.h,
                    }
                } else {
                    Rect {
                        x: r.x,
                        y: at,
                        w: r.w,
                        h: size,
                    }
                };
                children.push(json_node(child, sub, cells));
                at += size + 1;
            }
            json!({
                "t": if *side_by_side { "h" } else { "v" },
                "w": r.w, "h": r.h, "x": r.x, "y": r.y, "c": children,
            })
        }
    }
}

const SEP: char = '\t';

/// The window `target` names (`@id`, `session:index`, or a pane in it).
pub fn window(target: &str) -> Result<Window> {
    let head = cmd::run(
        "tmux",
        &[
            "display-message",
            "-p",
            "-t",
            target,
            "#{window_id}\t#{window_width}\t#{window_height}\t#{window_zoomed_flag}",
        ],
        cmd::FAST,
    )?;
    let f: Vec<&str> = head.trim_end().split(SEP).collect();
    if f.len() < 4 || f[0].is_empty() {
        bail!("{target}: no such window");
    }
    let raw = cmd::run(
        "tmux",
        &[
            "list-panes",
            "-t",
            f[0],
            "-F",
            "#{pane_index}\t#{pane_id}\t#{pane_pid}\t#{?pane_active,1,0}\t#{@cc_pane_session}\t#{pane_title}",
        ],
        cmd::FAST,
    )?;
    let tree = proc::Tree::capture_with_args().context("read the process tree")?;
    let cells = raw
        .lines()
        .filter_map(|line| {
            let p: Vec<&str> = line.splitn(6, SEP).collect();
            if p.len() < 6 {
                return None;
            }
            let pid: u32 = p[2].parse().ok()?;
            let line = command::for_pane(pid, &tree, |pid| tree.args(pid).map(str::to_string));
            // Only the pane's own id: the window's `@cc_session` is shared by
            // every pane in the window, the browser beside claude included.
            let kind = kind_of(line.as_deref(), !p[4].is_empty());
            Some(Cell {
                index: p[0].parse().ok()?,
                id: p[1].to_string(),
                label: label(p[5], line.as_deref(), &kind),
                kind,
                active: p[3] == "1",
            })
        })
        .collect();
    Ok(Window {
        id: f[0].to_string(),
        width: f[1].parse().context("window width")?,
        height: f[2].parse().context("window height")?,
        zoomed: f[3] == "1",
        cells,
    })
}

/// A title worth showing: claude's spinner glyph and the host name tmux
/// puts in an untitled pane say nothing, the command says more.
fn label(title: &str, command: Option<&str>, kind: &str) -> String {
    let title = title
        .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '~')
        .trim();
    let host_like =
        !title.contains(' ') && title.contains('-') && title.chars().any(|c| c.is_uppercase());
    if !title.is_empty() && !host_like {
        return title.to_string();
    }
    command
        .map(str::to_string)
        .unwrap_or_else(|| kind.to_string())
}

/// Lay `win` out by `preset`. A zoomed window is unzoomed first: tmux
/// refuses a layout change while one pane fills the window.
pub fn apply(win: &Window, preset: &Preset) -> Result<String> {
    let layout = to_json(&preset.tree(&win.cells), win)?;
    if win.zoomed {
        cmd::run("tmux", &["resize-pane", "-Z", "-t", &win.id], cmd::FAST)?;
    }
    cmd::run(
        "tmux",
        &["select-layout", "-t", &win.id, &layout],
        cmd::FAST,
    )
    .with_context(|| format!("select-layout {}", preset.name))?;
    Ok(layout)
}

#[cfg(test)]
mod tests;
