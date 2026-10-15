//! Everything that shells out, off the input thread.
//!
//! A reload is `ps` plus `tmux list-panes` — ~0.1 s on a good day, up to the
//! 3 s command timeout on a bad one — and a restore waits for shells to come
//! up window by window. Run on the input thread, either froze Esc for as long
//! as it took. Here a worker thread runs them and posts the result back over
//! a channel; the event loop polls that channel between keys.
//!
//! Every reply carries the generation it was requested under. The loop drops
//! a reload whose generation has been superseded, so a slow tick that started
//! before `p` switched the restore point cannot paint the old point's diff
//! over the new one.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use anyhow::{Context, Result, anyhow};

use super::model::Model;
use crate::collect::{notify, proc, tmux};
use crate::layout::Session;
use crate::layout::point::{self, Point, Ref};

/// One reading of the world: what a reload needs to rebuild the tree.
pub struct Snapshot {
    panes: Vec<tmux::Pane>,
    tree: proc::Tree,
    pending: HashMap<String, notify::Entry>,
    saved: Vec<Session>,
}

impl Snapshot {
    /// Read tmux, the process table, the queue and the restore point.
    ///
    /// All or nothing: a failed `tmux list-panes` used to read as "no
    /// panes", which wiped the tree and marked every window in the point as
    /// gone until the next tick.
    pub fn read(point: Option<&Ref>) -> Result<Self> {
        let panes = tmux::panes().context("list tmux panes")?;
        let tree = proc::Tree::capture_with_args().context("read the process table")?;
        let pending = notify::load();
        let saved = match point {
            Some(r) => point::read(r).unwrap_or_default(),
            None => Vec::new(),
        };
        Ok(Self {
            panes,
            tree,
            pending,
            saved,
        })
    }

    /// A reading of one live window with one pane, for tests that need the
    /// loop to apply something without a tmux server.
    #[cfg(test)]
    pub fn for_test(session: &str, window: u32) -> Self {
        Self {
            panes: vec![tmux::Pane {
                session: session.into(),
                session_attached: true,
                window_index: window,
                window_id: format!("@{window}"),
                window_name: format!("w{window}"),
                window_layout: String::new(),
                window_active: true,
                pane_index: 0,
                pane_id: format!("%{window}"),
                pid: 0,
                path: "/".into(),
                current_command: "zsh".into(),
                active: true,
                cc_session: String::new(),
                cc_state: String::new(),
            }],
            tree: proc::Tree::parse_for_test(""),
            pending: HashMap::new(),
            saved: Vec::new(),
        }
    }

    /// Rebuild the model's tree from this reading. Cheap and I/O-free, so
    /// expanding a window re-runs it on the input thread.
    pub fn apply(&self, model: &mut Model) {
        model.refresh(&self.panes, &self.saved, &self.tree, &self.pending);
    }
}

/// What a finished command does to the model, on the input thread, after
/// the fresh snapshot is applied — so a status like "r brings back 2 of
/// them" is computed against the tree it describes.
pub type Finish = Box<dyn FnOnce(&mut Model) + Send>;

/// A command's result.
pub struct Done {
    pub finish: Finish,
    /// Set when the command changed the list of restore points (a save); the
    /// newest becomes the one compared against.
    pub points: Option<Vec<Point>>,
}

impl Done {
    pub fn status(level: super::status::Level, msg: impl Into<String>) -> Self {
        let msg = msg.into();
        Self::then(move |m| m.status.set(level, msg))
    }

    pub fn then(f: impl FnOnce(&mut Model) + Send + 'static) -> Self {
        Self {
            finish: Box::new(f),
            points: None,
        }
    }
}

/// A command that changes tmux, prepared on the input thread from what was
/// on screen and run on the worker.
pub struct Job {
    /// Shown while it runs: "restoring", "saving".
    pub label: &'static str,
    pub task: Box<dyn FnOnce() -> Result<Done> + Send>,
}

impl Job {
    pub fn new(label: &'static str, task: impl FnOnce() -> Result<Done> + Send + 'static) -> Self {
        Self {
            label,
            task: Box::new(task),
        }
    }
}

enum Request {
    Reload {
        generation: u64,
        point: Option<Ref>,
    },
    Job {
        generation: u64,
        point: Option<Ref>,
        job: Job,
    },
}

pub enum Reply {
    Reloaded {
        generation: u64,
        snapshot: Result<Snapshot>,
    },
    JobDone {
        generation: u64,
        done: Result<Done>,
        snapshot: Result<Snapshot>,
    },
    Preview {
        target: String,
        body: String,
    },
}

/// The two worker threads and the channel they answer on.
///
/// Previews get a thread of their own: a capture is what the cursor waits on,
/// and queued behind a restore it would leave the panel blank for seconds.
pub struct Workers {
    io: Sender<Request>,
    preview: Sender<String>,
    replies: Receiver<Reply>,
}

impl Workers {
    pub fn start() -> Self {
        let (reply_tx, replies) = channel();

        let (io, io_rx) = channel::<Request>();
        let tx = reply_tx.clone();
        thread::spawn(move || {
            for request in io_rx {
                let reply = match request {
                    Request::Reload { generation, point } => Reply::Reloaded {
                        generation,
                        snapshot: guarded(|| Snapshot::read(point.as_ref())),
                    },
                    Request::Job {
                        generation,
                        point,
                        job,
                    } => {
                        let done = guarded(job.task);
                        // Read the world after the command, against the new
                        // newest point if it made one.
                        let point = match &done {
                            Ok(Done {
                                points: Some(list), ..
                            }) => list.first().map(|p| p.reference.clone()),
                            _ => point,
                        };
                        Reply::JobDone {
                            generation,
                            done,
                            snapshot: guarded(|| Snapshot::read(point.as_ref())),
                        }
                    }
                };
                if tx.send(reply).is_err() {
                    return; // the UI is gone
                }
            }
        });

        let (preview, preview_rx) = channel::<String>();
        thread::spawn(move || {
            for target in preview_rx {
                // A failed capture shows as an empty panel, as it always has:
                // the preview is context, and an error in its place would be
                // louder than the thing it failed to show.
                let body = tmux::capture_pane(&target, 40).unwrap_or_default();
                if reply_tx.send(Reply::Preview { target, body }).is_err() {
                    return;
                }
            }
        });

        Self {
            io,
            preview,
            replies,
        }
    }

    pub fn reload(&self, generation: u64, point: Option<Ref>) {
        // A send only fails once the thread has exited, which it does only
        // when this struct is dropped; there is nobody left to tell.
        let _ = self.io.send(Request::Reload { generation, point });
    }

    pub fn job(&self, generation: u64, point: Option<Ref>, job: Job) {
        let _ = self.io.send(Request::Job {
            generation,
            point,
            job,
        });
    }

    pub fn capture(&self, target: String) {
        let _ = self.preview.send(target);
    }

    pub fn try_recv(&self) -> Option<Reply> {
        self.replies.try_recv().ok()
    }

    /// Block until the next reply. Only for leaving while a command runs.
    pub fn recv(&self) -> Option<Reply> {
        self.replies.recv().ok()
    }
}

/// Run `f`, turning a panic into an error the status line can show.
///
/// A panic on the worker must not take the thread down: every later reload
/// would then silently never answer and the tree would freeze.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(anyhow!("internal error: {msg}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panicking_task_becomes_an_error_not_a_dead_thread() {
        let r: Result<()> = guarded(|| panic!("boom"));
        assert!(format!("{:#}", r.unwrap_err()).contains("boom"));
    }

    #[test]
    fn a_job_reply_comes_back_tagged_with_its_generation() {
        let w = Workers::start();
        w.job(
            7,
            None,
            Job::new("testing", || {
                Ok(Done::status(super::super::status::Level::Success, "done"))
            }),
        );
        match w.recv() {
            Some(Reply::JobDone {
                generation, done, ..
            }) => {
                assert_eq!(generation, 7);
                let mut m = Model::new(Vec::new());
                (done.expect("ok").finish)(&mut m);
                assert_eq!(m.status, "done");
            }
            _ => panic!("expected a job reply"),
        }
    }
}
