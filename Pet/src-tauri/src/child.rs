//! A child process the pet talks to over its stdin: the conversation core and the speech input
//! service (core.rs, stt.rs). What they share is how one run of it ends so the next can start with
//! new settings (Docs/DESIGN_Settings.md 4): its stdin is closed, which both read as the host being
//! gone and exit on, it is given a bounded wait, and one still running after that is killed.
//!
//! Each run has a number. The thread that reads a run's stdout says the child is gone only while
//! that run is still the current one, so the end of a run a restart retired cannot mark the run
//! that replaced it as down, or take its stdin away.

use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// How long a child has to exit on its own once its stdin is closed. The core ends the turn it is
/// in and the speech input service the press it holds, and both take milliseconds to do it; five
/// seconds is for a machine that is busy, and still short enough for the user waiting on a restart.
pub const STOP_WAIT: Duration = Duration::from_secs(5);
/// After a kill, how long until the run is given up on. Its reader then ends on its own time.
const KILL_WAIT: Duration = Duration::from_secs(2);
/// How often the reader asks whether a child whose stdout has closed has exited yet. Polled, and
/// not a blocking wait, so a stop that has to kill it never waits on a lock the reader holds.
const REAP_POLL: Duration = Duration::from_millis(20);

#[derive(Default)]
pub struct Piped {
    stdin: Mutex<Option<ChildStdin>>,
    run: Mutex<Option<Run>>,
    runs: AtomicU64,
}

struct Run {
    id: u64,
    child: Arc<Mutex<Child>>,
    exited: Receiver<()>,
}

/// What the thread reading a run's stdout keeps, to wait for the run to exit once its stdout ends.
pub struct Reaper {
    id: u64,
    child: Arc<Mutex<Child>>,
    exited: Sender<()>,
}

/// How a stop went, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// There was no run to stop.
    Idle,
    /// It exited on its own once its stdin closed.
    Exited,
    /// It did not, and was killed.
    Killed,
    /// It was killed and still had not exited when the wait ran out.
    Stuck,
}

impl Piped {
    /// Makes a newly spawned child the current run, and its stdin the one `stdin` hands out.
    pub fn adopt(&self, stdin: ChildStdin, child: Child) -> Reaper {
        let id = self.runs.fetch_add(1, Ordering::Relaxed);
        let child = Arc::new(Mutex::new(child));
        let (tx, rx) = mpsc::channel();
        *self.run.lock().expect("run poisoned") = Some(Run { id, child: Arc::clone(&child), exited: rx });
        *self.stdin.lock().expect("stdin poisoned") = Some(stdin);
        Reaper { id, child, exited: tx }
    }

    /// The current run's stdin, `None` when there is none. Held for one whole line, so two lines
    /// written at once cannot interleave.
    pub fn stdin(&self) -> MutexGuard<'_, Option<ChildStdin>> {
        self.stdin.lock().expect("stdin poisoned")
    }

    /// Called by a run's reader once the child has exited. `gone` runs, and the stdin is dropped, only
    /// when that run is still the current one: a run a stop retired has a restart that says what
    /// happens next. `gone` runs before the stdin goes, so a line that finds no stdin finds the reason.
    pub fn end(&self, reaper: &Reaper, gone: impl FnOnce()) {
        let mut run = self.run.lock().expect("run poisoned");
        if run.as_ref().is_some_and(|r| r.id == reaper.id) {
            gone();
            *run = None;
            *self.stdin.lock().expect("stdin poisoned") = None;
        }
    }

    /// Ends the current run: retired first, so its reader does not report it gone, then its stdin
    /// closed, then a wait of up to `wait` for it to exit, then a kill. Idempotent: with no run it
    /// does nothing.
    pub fn stop(&self, wait: Duration) -> Stopped {
        let Some(run) = self.run.lock().expect("run poisoned").take() else {
            return Stopped::Idle;
        };
        drop(self.stdin.lock().expect("stdin poisoned").take());
        match run.exited.recv_timeout(wait) {
            Ok(()) => return Stopped::Exited,
            // The reader is gone without saying so, which leaves the child's state unknown, so it is
            // killed like one that did not exit.
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        if let Err(e) = run.child.lock().expect("child poisoned").kill() {
            eprintln!("child: kill: {e}");
        }
        match run.exited.recv_timeout(KILL_WAIT) {
            Ok(()) => Stopped::Killed,
            Err(_) => Stopped::Stuck,
        }
    }
}

impl Reaper {
    /// Waits for the child to exit, once its stdout has ended, and tells a stop waiting on it.
    /// Returns its exit code, `None` when it has none or cannot be read.
    pub fn wait(&self) -> Option<i32> {
        let code = loop {
            match self.child.lock().expect("child poisoned").try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("child: try_wait: {e}");
                    break None;
                }
            }
            std::thread::sleep(REAP_POLL);
        };
        let _ = self.exited.send(());
        code
    }
}
