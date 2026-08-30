//! The tmux paste-buffer history, and copy-mode commands.
//!
//! Absorbed from `tmux-fzf`, which put both behind a two-level menu. These are
//! the only two of its eight entries worth keeping: the other six — session,
//! window, pane, process, keybinding, command — are covered better by the tree
//! here, by `tpx`, or by the existing `?` binding.

use anyhow::Result;

use crate::collect::cmd;

/// One entry in the tmux paste-buffer stack.
#[derive(Clone, Debug, PartialEq)]
pub struct Buffer {
    /// `buffer1047`, the name `paste-buffer -b` takes.
    pub name: String,
    pub bytes: usize,
    /// First line, whitespace-collapsed, for the picker.
    pub preview: String,
}

/// The paste buffers, newest first.
///
/// `copyq` is not installed here, so only tmux's own stack is read — that is
/// where the 100 buffers on this machine live.
pub fn buffers() -> Result<Vec<Buffer>> {
    // A custom format avoids parsing `list-buffers`' human-readable line,
    // which quotes and escapes the sample in ways that are painful to undo.
    let raw = cmd::run(
        "tmux",
        &[
            "list-buffers",
            "-F",
            "#{buffer_name}\t#{buffer_size}\t#{buffer_sample}",
        ],
        cmd::FAST,
    )?;
    Ok(parse_buffers(&raw))
}

fn parse_buffers(raw: &str) -> Vec<Buffer> {
    raw.lines().filter_map(parse_buffer).collect()
}

fn parse_buffer(line: &str) -> Option<Buffer> {
    let mut fields = line.splitn(3, '\t');
    let name = fields.next()?.to_string();
    let bytes = fields.next()?.parse().ok()?;
    let preview = collapse(fields.next().unwrap_or(""));
    Some(Buffer {
        name,
        bytes,
        preview,
    })
}

/// Squash runs of whitespace so the preview stays on one row.
///
/// tmux already strips newlines from `buffer_sample` — verified against the
/// 100 buffers on this machine, where the listing has exactly one line per
/// buffer. What is left to handle is tabs and multiple spaces, which would
/// otherwise misalign the column.
fn collapse(sample: &str) -> String {
    let mut out = String::new();
    let mut in_space = false;
    for ch in sample.chars() {
        if ch.is_whitespace() {
            if !in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out.trim_end().to_string()
}

/// Paste a buffer into a pane.
pub fn paste(buffer: &str, target: &str) -> Result<()> {
    cmd::run(
        "tmux",
        &["paste-buffer", "-b", buffer, "-t", target],
        cmd::FAST,
    )?;
    Ok(())
}

/// A copy-mode command, as `send-keys -X` takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CopyCommand {
    pub name: &'static str,
    pub about: &'static str,
}

/// The copy-mode commands worth offering.
///
/// tmux-fzf listed all 44, including every cursor movement — `cursor-left`,
/// `next-word` — which nobody reaches for through a picker when the key is
/// right there. What is left is what is hard to remember or has no binding.
pub const COPY_COMMANDS: &[CopyCommand] = &[
    CopyCommand {
        name: "begin-selection",
        about: "start selecting",
    },
    CopyCommand {
        name: "rectangle-toggle",
        about: "block selection",
    },
    CopyCommand {
        name: "select-line",
        about: "select the whole line",
    },
    CopyCommand {
        name: "select-word",
        about: "select the word",
    },
    CopyCommand {
        name: "copy-selection",
        about: "copy, stay in copy-mode",
    },
    CopyCommand {
        name: "copy-selection-and-cancel",
        about: "copy and leave",
    },
    CopyCommand {
        name: "copy-line",
        about: "copy the whole line",
    },
    CopyCommand {
        name: "copy-end-of-line",
        about: "copy to end of line",
    },
    CopyCommand {
        name: "search-forward",
        about: "search down",
    },
    CopyCommand {
        name: "search-backward",
        about: "search up",
    },
    CopyCommand {
        name: "search-again",
        about: "repeat the search",
    },
    CopyCommand {
        name: "next-matching-bracket",
        about: "jump to the pair",
    },
    CopyCommand {
        name: "next-paragraph",
        about: "paragraph down",
    },
    CopyCommand {
        name: "previous-paragraph",
        about: "paragraph up",
    },
    CopyCommand {
        name: "history-top",
        about: "top of scrollback",
    },
    CopyCommand {
        name: "history-bottom",
        about: "bottom of scrollback",
    },
    CopyCommand {
        name: "goto-line",
        about: "jump to a line number",
    },
    CopyCommand {
        name: "jump-to-mark",
        about: "back to the mark",
    },
    CopyCommand {
        name: "clear-selection",
        about: "drop the selection",
    },
    CopyCommand {
        name: "cancel",
        about: "leave copy-mode",
    },
];

/// Send a copy-mode command to a pane.
pub fn send_copy_command(name: &str, target: &str) -> Result<()> {
    cmd::run("tmux", &["send-keys", "-X", "-t", target, name], cmd::FAST)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_buffer_listing() {
        let raw = "buffer1047\t40\t/session-policy allow tmux_window_escape\n\
                   buffer1045\t93\talias happy \"ccproxy claude\"";
        let buffers = parse_buffers(raw);

        assert_eq!(buffers.len(), 2);
        assert_eq!(buffers[0].name, "buffer1047");
        assert_eq!(buffers[0].bytes, 40);
        assert_eq!(
            buffers[0].preview,
            "/session-policy allow tmux_window_escape"
        );
    }

    #[test]
    fn runs_of_whitespace_collapse_so_the_column_stays_aligned() {
        let b = parse_buffer("buffer1\t20\tspaced    out\tand\t\ttabbed").unwrap();
        assert_eq!(b.preview, "spaced out and tabbed");
    }

    #[test]
    fn a_buffer_whose_sample_contains_a_tab_keeps_it_all() {
        // splitn(3) means everything after the size is the sample, tabs and
        // all — a two-field split would truncate at the first one.
        let b = parse_buffer("buffer1\t10\ta\tb\tc").unwrap();
        assert_eq!(b.preview, "a b c");
    }

    #[test]
    fn a_malformed_row_is_dropped_not_fatal() {
        let raw = "buffer1\t10\tok\ngarbage\nbuffer2\tnot-a-number\tx\nbuffer3\t5\tfine";
        assert_eq!(parse_buffers(raw).len(), 2);
    }

    #[test]
    fn an_empty_buffer_still_lists() {
        let b = parse_buffer("buffer1\t0\t").unwrap();
        assert_eq!(b.bytes, 0);
        assert!(b.preview.is_empty());
    }

    #[test]
    fn the_copy_command_list_leaves_out_plain_cursor_movement() {
        let names: Vec<&str> = COPY_COMMANDS.iter().map(|c| c.name).collect();
        for movement in ["cursor-left", "cursor-down", "next-word", "start-of-line"] {
            assert!(
                !names.contains(&movement),
                "{movement} has a key already; a picker entry for it is noise",
            );
        }
        assert!(names.contains(&"copy-selection-and-cancel"));
        assert!(names.contains(&"rectangle-toggle"));
    }
}
