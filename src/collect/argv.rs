//! A process's real argument vector, and the shell spelling that types it back.
//!
//! `ps -o args` joins argv with spaces. That loses every argument boundary
//! and every quote the user typed: `claude --settings '{"a":{"b":true}}'`
//! comes back as `claude --settings {"a":{"b":true}}`, and typing that into
//! fish brace-expands the JSON into `a:b:true` before claude ever sees it.
//! The vector itself is still available from the kernel, so restore reads it
//! from there and quotes each argument that needs it.

/// The argument vector of `pid`, or `None` when the kernel will not give it
/// (the process exited, or belongs to another user).
pub fn read(pid: u32) -> Option<Vec<String>> {
    let argv = os::read(pid)?;
    if argv.is_empty() { None } else { Some(argv) }
}

/// Join an argument vector into one line that fish, zsh and bash all parse
/// back into the same vector.
pub fn join(argv: &[String]) -> String {
    argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
}

/// Characters every supported shell reads literally, anywhere in a word.
fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '@' | ',' | '+' | '=')
}

/// Quote one argument.
///
/// Plain words stay bare so the common command line reads as it was typed.
/// Anything else goes in single quotes, with `'` and `\` written *outside*
/// the quotes as `\'` and `\\`: POSIX shells treat a backslash inside single
/// quotes literally while fish treats `\\` and `\'` there as escapes, so only
/// the outside spelling means the same thing in both.
///
/// A leading `=` is quoted too — zsh expands `=word` to the path of `word`.
fn quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if !arg.starts_with('=') && arg.chars().all(is_plain) {
        return arg.to_string();
    }
    let mut out = String::new();
    let mut open = false;
    for c in arg.chars() {
        if c == '\'' || c == '\\' {
            if open {
                out.push('\'');
                open = false;
            }
            out.push('\\');
            out.push(c);
        } else {
            if !open {
                out.push('\'');
                open = true;
            }
            out.push(c);
        }
    }
    if open {
        out.push('\'');
    }
    out
}

/// Split a command line into words, keeping each word's spelling as written.
///
/// Understands exactly what `quote` produces — single quotes and a backslash
/// outside them — so a quoted argument holding a space or a `--` stays one
/// word. A line with an unterminated quote is not one `quote` wrote (an old
/// snapshot, or `ps` text with a stray apostrophe), and splits on spaces as
/// before.
pub fn split_words(line: &str) -> Vec<&str> {
    split_quoted(line).unwrap_or_else(|| line.split(' ').collect())
}

fn split_quoted(line: &str) -> Option<Vec<&str>> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    let mut in_quote = false;
    let mut chars = line.char_indices();
    while let Some((i, c)) = chars.next() {
        if in_quote {
            if c == '\'' {
                in_quote = false;
            }
            continue;
        }
        match c {
            ' ' => {
                if let Some(s) = start.take() {
                    words.push(&line[s..i]);
                }
            }
            '\'' => {
                start.get_or_insert(i);
                in_quote = true;
            }
            '\\' => {
                start.get_or_insert(i);
                chars.next();
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if in_quote {
        return None;
    }
    if let Some(s) = start {
        words.push(&line[s..]);
    }
    Some(words)
}

#[cfg(target_os = "macos")]
mod os {
    /// `sysctl(KERN_PROCARGS2)`: an `int` argc, the executable path, NUL
    /// padding, then argc NUL-terminated arguments (the environment follows
    /// and is ignored).
    pub fn read(pid: u32) -> Option<Vec<String>> {
        let mut argmax: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        // SAFETY: mib names a readable int and `size` matches the buffer.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                (&raw mut argmax).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || argmax <= 0 {
            return None;
        }
        let mut buf = vec![0u8; argmax as usize];
        let mut size = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
        // SAFETY: `buf` is `size` bytes long; the kernel writes at most that.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        parse(&buf[..size])
    }

    pub(super) fn parse(buf: &[u8]) -> Option<Vec<String>> {
        let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?);
        let mut rest = &buf[4..];
        // Executable path, then the NUL padding after it.
        let path_end = rest.iter().position(|&b| b == 0)?;
        rest = &rest[path_end..];
        let first = rest.iter().position(|&b| b != 0)?;
        rest = &rest[first..];
        let mut argv = Vec::new();
        for _ in 0..argc.max(0) {
            let end = rest.iter().position(|&b| b == 0)?;
            argv.push(String::from_utf8_lossy(&rest[..end]).into_owned());
            rest = &rest[end + 1..];
        }
        Some(argv)
    }
}

#[cfg(target_os = "linux")]
mod os {
    pub fn read(pid: u32) -> Option<Vec<String>> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let raw = raw.strip_suffix(&[0]).unwrap_or(&raw);
        Some(
            raw.split(|&b| b == 0)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect(),
        )
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod os {
    pub fn read(_pid: u32) -> Option<Vec<String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plain_arguments_stay_bare() {
        let line = join(&v(&["claude", "--model", "default", "--resume", "abc-123"]));
        assert_eq!(line, "claude --model default --resume abc-123");
    }

    #[test]
    fn json_settings_are_quoted() {
        let line = join(&v(&[
            "claude",
            "--settings",
            r#"{"enabledPlugins":{"cco@claude-code-plugin":true}}"#,
        ]));
        assert_eq!(
            line,
            r#"claude --settings '{"enabledPlugins":{"cco@claude-code-plugin":true}}'"#
        );
    }

    #[test]
    fn quotes_and_backslashes_go_outside_the_quotes() {
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(r"a\b"), r"'a'\\'b'");
        assert_eq!(quote("'"), r"\'");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("=ls"), "'=ls'");
    }

    #[test]
    fn split_keeps_a_quoted_word_whole() {
        let line = join(&v(&[
            "claude",
            "--append-system-prompt",
            "a -- b",
            "--",
            "--x",
        ]));
        assert_eq!(
            split_words(&line),
            vec!["claude", "--append-system-prompt", "'a -- b'", "--", "--x"]
        );
        assert_eq!(split_words(r"x 'it'\''s' y"), vec!["x", r"'it'\''s'", "y"]);
    }

    #[test]
    fn an_unterminated_quote_splits_on_spaces() {
        assert_eq!(
            split_words("echo don't stop"),
            vec!["echo", "don't", "stop"]
        );
    }

    /// The round trip that matters: the quoted line must parse back into the
    /// same argv in every shell restore can type into.
    #[test]
    fn every_shell_reads_the_quoted_line_back() {
        let argv = v(&[
            "printf",
            "%s\\n",
            r#"{"a":{"b":true},"c":[1,2]}"#,
            "it's",
            r"back\\slash\'",
            "a b",
            "*",
            "$HOME",
            "",
        ]);
        let line = join(&argv);
        let want: String = argv[2..].iter().map(|a| format!("{a}\n")).collect();
        for shell in ["sh", "bash", "zsh", "fish"] {
            let Ok(out) = std::process::Command::new(shell)
                .arg("-c")
                .arg(&line)
                .output()
            else {
                continue;
            };
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                want,
                "{shell}: {line}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_procargs2() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/claude\0\0\0claude\0{\"a\":1}\0HOME=/x\0");
        assert_eq!(os::parse(&buf), Some(v(&["claude", r#"{"a":1}"#])));
    }

    #[test]
    fn reads_this_process() {
        let argv = read(std::process::id()).expect("own argv");
        assert!(!argv[0].is_empty());
    }
}
