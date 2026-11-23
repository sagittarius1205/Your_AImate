//! The gatekeeper's own data: what a row may say about what a session is asking, which sessions are
//! the user's at all, and the order they are listed in. `Roster`, `Coverage`, the join and the reaping
//! move here from `main.rs` when that unit lands (Docs/DESIGN_Pet.md section 9).

use crate::Session;

pub mod pins;

/// How many characters of a notification a row may show.
///
/// A bound is needed, not preferred: `Notification`'s payload shape is not documented and has
/// fired 0 times so far (measured, Pet/README.md), so nothing says the message is short or even
/// single-line. One session must not be able to push the other nineteen off the panel.
///
/// 140 is picked so the line cannot run past about two rows at this panel's width. That is an
/// estimate from the font size and the window width, not a measurement, and the panel clamps it
/// in CSS as well so a wrong guess here is ugly rather than broken.
const MAX_CHARS: usize = 140;

/// What the row is allowed to show about what a session is asking, or `None` when there is nothing
/// worth a line.
///
/// This lives in Rust and not in the page for the reason the whole design gives: the WebView
/// renders, it does not decide (Docs/DESIGN_Pet.md section 3). "How much of a stranger's string may
/// appear in the panel" is a decision.
///
/// The input is a field of a JSON payload written by another process, so it is treated as hostile
/// text rather than as a sentence: every control character goes, every run of whitespace becomes
/// one space, and the result is bounded. A message that is only whitespace or only control
/// characters becomes `None`, so the row shows no line at all instead of an empty one that looks
/// like a rendering bug.
pub fn asking_line(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len().min(MAX_CHARS * 4));
    let mut pending_space = false;
    let mut chars = 0usize;
    let mut truncated = false;
    for c in raw.chars() {
        // `is_control` covers C0 and C1, which is what a newline, a tab, a NUL and an ANSI escape
        // all are. They are dropped rather than replaced: a message broken across lines should
        // read as one line, not as a line with gaps in it.
        if c.is_whitespace() || c.is_control() {
            // Only remember a space once there is something for it to follow, so a leading run of
            // whitespace cannot produce a line that starts with a space.
            pending_space = !out.is_empty();
            continue;
        }
        if chars >= MAX_CHARS {
            truncated = true;
            break;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
            chars += 1;
            if chars >= MAX_CHARS {
                truncated = true;
                break;
            }
        }
        out.push(c);
        chars += 1;
    }
    if out.is_empty() {
        return None;
    }
    if truncated {
        // One character, so the row says "there is more" without the page having to guess whether
        // the text ended or ran out of room.
        out.push('…');
    }
    Some(out)
}

/// The folder the pet's conversation core makes everything it runs under: `vrchatcore-<pid>` in the
/// temp folder (ChatCore/src/scratch.rs, `Scratch::under`). `pet_fleet` reads that file and goes red
/// when it stops spelling the name this way.
const SCRATCH_PREFIX: &str = "vrchatcore-";

/// Whether a session's working directory is one of the pet's own: anything under the core's scratch
/// folder, which is its conversation engine and the relay's courier today.
///
/// Both are `claude` processes, so `claude agents --json` lists them like sessions the user started,
/// under names the CLI makes up (`engine-dc`, `relay-probe2-f0`). They are not the user's, and
/// "17 sessions" with one of them the pet's own mouth is a wrong count. What the core controls is the
/// directory, so that is what is matched: any part of the path that is `vrchatcore-` and digits, in
/// either slash and any case. The scratch folder's own name is matched and not what is inside it, so a
/// third thing the core runs there is hidden without this changing. The folder above it is not
/// compared with the temp folder, because a short 8.3 spelling of it would make this miss, and that
/// mistake shows a row, where a stricter check could only hide one of the user's. A folder of the
/// user's own called `vrchatcore-<digits>` is hidden too, which is the price.
pub fn is_pet_engine(cwd: &str) -> bool {
    cwd.split(['\\', '/']).any(|part| {
        let part = part.to_ascii_lowercase();
        part.strip_prefix(SCRATCH_PREFIX).is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The four places a row can be listed, in the order they are listed. A waiting session is first
/// because that is the product, and a pinned one is next because the user said it matters. A busy one
/// is ahead of the rest because it is the other one doing something now. The derived `Ord` is the
/// order, so a fifth group is one more variant and cannot be forgotten at a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Waiting,
    Pinned,
    Busy,
    Rest,
}

pub fn group(s: &Session) -> Group {
    if s.waiting {
        Group::Waiting
    } else if s.pinned {
        Group::Pinned
    } else if s.status == "busy" {
        Group::Busy
    } else {
        Group::Rest
    }
}

/// Group by group, each newest first. Ties on the start time fall to the session id, so two polls of
/// the same fleet list it the same way and a row does not swap places with its twin.
pub fn order(rows: &mut [Session]) {
    rows.sort_by(|a, b| group(a).cmp(&group(b)).then(b.started_at.cmp(&a.started_at)).then_with(|| a.session_id.cmp(&b.session_id)));
}

/// The rows as the page lists them: each marked pinned or not by `pinned`, and in order. The order is
/// decided here and not in the page, for the reason the whole design gives: the page renders and
/// does not decide, and a rule that lives in Rust can be run by `pet_fleet`.
pub fn shown(polled: &[Session], pinned: &[String]) -> Vec<Session> {
    let mut rows: Vec<Session> = polled
        .iter()
        .cloned()
        .map(|mut s| {
            s.pinned = pinned.contains(&s.session_id);
            s
        })
        .collect();
    order(&mut rows);
    rows
}
