//! A press of the microphone button is a lease, and while it is alive the window must not turn
//! click-through (Docs/DESIGN_Voice.md 3.5).
//!
//! The poll thread in main.rs flips `set_ignore_cursor_events` every 40 ms from the cursor and the
//! page's rectangles. If it flipped the window to click-through while the button was held, the
//! window would never see the pointer go up, nor lose its capture, and the press would never end:
//! the microphone open for good. So the poll thread asks `pins_window` first.
//!
//! Pure, like hit.rs: no tauri type, so `pet_mic` drives it with no window.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::hit::Hit;

/// A lease nobody released ends by itself after this. Past the service's own 60 s cap, so the
/// service ends a stuck press first and this only covers a page that died holding one.
pub const MAX_HOLD: Duration = Duration::from_secs(65);

#[must_use = "dropping the lease is releasing the mic button"]
#[derive(Debug)]
pub struct PressLease {
    since: Instant,
}

#[derive(Debug, Default)]
pub struct Held(Mutex<Option<PressLease>>);

impl Held {
    /// True when this took the lease, false when one was already held. Pressing twice is one press.
    pub fn take(&self, now: Instant) -> bool {
        let mut slot = self.0.lock().expect("press lease poisoned");
        if slot.is_some() {
            return false;
        }
        *slot = Some(PressLease { since: now });
        true
    }

    /// True when a lease was released. Releasing twice is a no-op.
    pub fn release(&self) -> bool {
        self.0.lock().expect("press lease poisoned").take().is_some()
    }

    pub fn pins_window(&self) -> bool {
        self.0.lock().expect("press lease poisoned").is_some()
    }

    /// Ends a lease held longer than `MAX_HOLD`. True when it ended one, so the caller can tell the
    /// service to release.
    pub fn expire_overdue(&self, now: Instant) -> bool {
        let mut slot = self.0.lock().expect("press lease poisoned");
        if slot.as_ref().is_some_and(|l| now.saturating_duration_since(l.since) >= MAX_HOLD) {
            *slot = None;
            return true;
        }
        false
    }
}

/// The poll thread's one decision.
pub fn ignore_cursor(pinned: bool, hit: Hit) -> bool {
    !pinned && hit == Hit::Through
}
