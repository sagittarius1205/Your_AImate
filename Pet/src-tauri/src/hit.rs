//! Which part of the overlay the cursor is over, and whether a geometry report from the page is
//! believable. Pure on purpose: no tauri type appears here, so `--selftest=pet_hit` drives it with
//! no window (Docs/DESIGN_Pet.md sections 5.1 and 8).

use serde::{Deserialize, Serialize};

/// A rectangle as fractions of the window's viewport, reported by the page so that the hit test
/// and the drawing cannot disagree.
///
/// Fractions and not pixels, because the first version of this multiplied the page's CSS
/// rectangle by `scale_factor()` and was wrong: the six boundary points in Pet/README.md give
/// opposite answers between the two versions, and the first test passed anyway because it
/// checked the centre of the rectangle. A fraction of the viewport needs no DPI assumption to be
/// correct, which is the whole reason the protocol is fractions.
///
/// The explanation this comment used to carry is gone, because its measurement was wrong
/// (2026-11-03). It said `GetWindowRect` reports 460x620 physical, so CSS and physical pixels
/// were 1:1 here. A DPI-aware probe says 575x775 with `GetDpiForWindow` 120, which is 460x620
/// at exactly 1.25, so the old number came from a DPI-unaware process, where Windows virtualises
/// the rectangle. **Why the multiply was wrong is therefore not explained here any more**, and
/// the likely answer is that the harness measuring it was DPI-unaware too, so its cursor
/// coordinates were virtualised and the un-multiplied version looked right (inference, not
/// measured). Guessing at the mechanism is how the wrong explanation got written the first time.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// What the page last reported. Empty is the state before the first report and also the state of
/// a closed dialogue, and both mean the same thing, so there is no "reported" flag: with nothing
/// to hit, the whole window passes clicks through.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Regions {
    /// Dragging one moves the window.
    pub character: Vec<Region>,
    /// Clickable but never a drag: the dialogue's bubble, cards and log buttons.
    pub panel: Vec<Region>,
}

impl Regions {
    /// Takes the page's two lists as one report: both pass `validate` or `self` is left exactly as
    /// it was. Replacing a good report with a bad one would leave the window unclickable, or
    /// clickable in the wrong place, until the page happened to report again.
    pub fn accept(&mut self, character: &[Region], panel: &[Region]) -> Result<(), String> {
        let character = validate(character).map_err(|why| format!("character {why}"))?;
        let panel = validate(panel).map_err(|why| format!("panel {why}"))?;
        *self = Regions { character, panel };
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Through,
    Character,
    Panel,
}

/// The panel wins where it overlaps the character, because the panel is drawn on top: a press
/// there lands on a card, and answering it as a window drag would swallow the click
/// (Docs/DESIGN_Pet.md 5.1).
///
/// `cursor`, `origin` and `size` are all physical pixels, the window's viewport against the OS
/// cursor, so no DPI conversion happens anywhere. Edges are half open, as the spike's were.
pub fn hit(r: &Regions, cursor: (f64, f64), origin: (f64, f64), size: (f64, f64)) -> Hit {
    let inside = |g: &Region| {
        let left = origin.0 + g.x * size.0;
        let top = origin.1 + g.y * size.1;
        cursor.0 >= left && cursor.0 < left + g.w * size.0 && cursor.1 >= top && cursor.1 < top + g.h * size.1
    };
    if r.panel.iter().any(inside) {
        Hit::Panel
    } else if r.character.iter().any(inside) {
        Hit::Character
    } else {
        Hit::Through
    }
}

/// Where the cursor is in the window's own CSS pixels, which is what the page draws in, for the
/// figure to look toward. Off the window it is still reported, past the edges: she looks at the
/// cursor wherever it is, and the page could not see it there itself, because the window ignores
/// the mouse wherever clicks pass through. `cursor` and `origin` are physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Css {
    pub x: f64,
    pub y: f64,
}

pub fn css(cursor: (f64, f64), origin: (f64, f64), scale: f64) -> Css {
    Css { x: (cursor.0 - origin.0) / scale, y: (cursor.1 - origin.1) / scale }
}

/// Dividing two CSS pixel values leaves float noise: a rectangle whose right edge is the
/// viewport's can come out as 1.0000000000000002 and must still be accepted.
const EDGE_SLACK: f64 = 1e-6;

/// A report is taken whole or not at all. One bad rectangle means the page's geometry is wrong
/// somewhere, and half a report does not say which half to believe, so the caller keeps its last
/// good one (Docs/DESIGN_Pet.md 5.1). Empty is valid: a closed dialogue reports no panel.
pub fn validate(raw: &[Region]) -> Result<Vec<Region>, String> {
    for (at, r) in raw.iter().enumerate() {
        check(r).map_err(|why| format!("region {at} {r:?}: {why}"))?;
    }
    Ok(raw.to_vec())
}

fn check(r: &Region) -> Result<(), &'static str> {
    if ![r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite()) {
        return Err("a value is NaN or infinite");
    }
    if r.w <= 0.0 || r.h <= 0.0 {
        return Err("width and height must be positive");
    }
    if r.x < -EDGE_SLACK || r.y < -EDGE_SLACK || r.x + r.w > 1.0 + EDGE_SLACK || r.y + r.h > 1.0 + EDGE_SLACK {
        return Err("it extends outside the viewport");
    }
    Ok(())
}
