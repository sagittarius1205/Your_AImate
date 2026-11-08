//! The gatekeeper's own data. Today only the one decision the roster row needs; `Roster`,
//! `Coverage`, the join and the reaping move here from `main.rs` when that unit lands
//! (Docs/DESIGN_Pet.md section 9).

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
