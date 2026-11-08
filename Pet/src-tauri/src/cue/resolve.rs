//! Pure: cues and bindings in, an ordered schedule and one disposition per cue out. No IO, no
//! clock, no window, so `--selftest=pet_cues` can drive it directly.

use serde::Serialize;
use vrcue::{Binding, Bindings, CharacterAction, Cue, CueDisposition, CueKind, Degradation, Unresolved, resolve};

/// What asked for a cue, which is what decides who keeps the hands when two want them at once.
///
/// Placement order, and it is not stroke order. `VRGesturePlan.cpp:378-385` sorts candidates by
/// priority before time: a tag is 2, a question mark 1, a keyword rule 0, and a beat -1, with the
/// louder beat first. So **every cue the words asked for is placed before any beat**, and the
/// comment at :362 says it outright: beats take "whatever room the words leave".
///
/// Sorting by stroke time alone, which is what this function first did, let a beat at 1.0 s take
/// the hands and suppress an `Offer` at 1.2 s. The planner keeps the `Offer`.
///
/// A tag and a question mark are two claims and not one. The pet used to receive its cues from the
/// core and could not tell them apart; it now plans them itself (`plan.rs`), and a `[offer]` that
/// follows a question would lose its hands to the question mark if the two ranked alike. The
/// keyword rules are not ported (`plan.rs` says why), so priority 0 has no variant.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Claim {
    /// A `[tag]` the core put in the text. Priority 2.
    Tag,
    /// A clause that ends in a question mark (VRGesturePlan.cpp:354-361). Priority 1.
    QuestionMark,
    /// A stressed syllable this loud (VRGesturePlan.cpp:362-377). Priority -1, loudest first.
    Stress(f32),
}

impl Claim {
    /// Rank, then the order within it: earlier stroke first, except beats, which go loudest first.
    /// Input position settles what is left, as the planner's `StableSort` does.
    fn place(self, at: f32) -> (u8, f32) {
        match self {
            Claim::Tag => (0, at),
            Claim::QuestionMark => (1, at),
            Claim::Stress(loudness) => (2, -loudness),
        }
    }
}

/// An action for the page to apply when the utterance clock reaches `at`.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct ScheduledAction {
    pub at: f32,
    pub action: CharacterAction,
}

struct Candidate {
    index: usize,
    kind: CueKind,
    at: f32,
    place: (u8, f32),
    action: CharacterAction,
    degraded: Option<Degradation>,
}

impl Candidate {
    /// Whether this cue takes the hands for its interval. A cue on another channel does not, and
    /// neither does one whose action is `Nothing`: no hand moves for it, so there is no twitching
    /// to prevent and no reason for it to cost a real gesture its place.
    fn reserves_hands(&self) -> bool {
        self.kind.occupies().exclusive() && self.action != CharacterAction::Nothing
    }

    /// The reserved interval of a cue at stroke `t` is `[t - rise, t + fall)`. `hold` is not added:
    /// the planner never added it either (VRGesturePlan.cpp:391-397), and adding it would make
    /// gestures that were kept apart collide.
    fn collides_with(&self, other: &Candidate) -> bool {
        let (mine, theirs) = (self.kind.span(), other.kind.span());
        self.kind.occupies() == other.kind.occupies()
            && self.at - mine.rise < other.at + theirs.fall
            && other.at - theirs.rise < self.at + mine.fall
    }
}

/// Drops cues whose reserved interval collides on the same channel, and returns the surviving
/// actions in stroke order plus exactly one disposition per input cue, in input order.
///
/// **Placement order and output order are two different things.** Who wins a collision is decided
/// in `Claim` order; the actions that survive come back sorted by stroke, because that is the only
/// order the page can play them in.
///
/// A cue nobody has bound is `Unresolved` and never reaches the schedule. One the user declared the
/// model cannot do is `Unavailable`, and it still carries its deliberate `Nothing` action so the
/// page's third branch stays reachable. Anything else is `Played`, or `Degraded` when its binding
/// is, unless a cue placed before it holds the hands (`Suppressed`). A suppressed cue reserves
/// nothing, so it cannot suppress a third one.
pub fn schedule(cues: &[(Cue, Claim)], bindings: &Bindings) -> (Vec<ScheduledAction>, Vec<CueDisposition>) {
    let mut decided: Vec<(usize, CueDisposition)> = Vec::with_capacity(cues.len());
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut deliberate_nothings: Vec<ScheduledAction> = Vec::new();
    for (index, (cue, claim)) in cues.iter().enumerate() {
        let kind = cue.kind();
        // `Unresolved` and `Unavailable` are kept apart here for the same reason `resolve` returns
        // Err for one and Ok for the other: "nobody decided" is a gap the binding UI must nag
        // about, while "the model cannot do it" is a decision already taken. Reporting both as
        // `Unavailable` would put the silent failure back one layer down, where `pet_coverage`
        // reads, and that test's job is to tell a missing arm from a missing decision.
        //
        // The binding is read only to classify. `resolve` stays the one place a cue becomes an
        // action, so a declared-absent cue's `Nothing` comes from there like every other action.
        let declared_absent = matches!(bindings[kind], Binding::DeclaredAbsent);
        match resolve(cue, bindings) {
            Err(Unresolved::Unbound) => decided.push((index, CueDisposition::Unresolved { kind })),
            Ok(action) if declared_absent => {
                decided.push((index, CueDisposition::Unavailable { kind }));
                deliberate_nothings.push(ScheduledAction { at: cue.at(), action });
            }
            Ok(action) => candidates.push(Candidate {
                index,
                kind,
                at: cue.at(),
                place: claim.place(cue.at()),
                action,
                degraded: bindings[kind].degradation(),
            }),
        }
    }
    candidates.sort_by(|a, b| {
        (a.place.0.cmp(&b.place.0)).then(a.place.1.total_cmp(&b.place.1)).then(a.index.cmp(&b.index))
    });

    let mut scheduled = Vec::with_capacity(candidates.len());
    let mut holding_hands: Vec<usize> = Vec::new();
    for (position, c) in candidates.iter().enumerate() {
        let blocker = if c.reserves_hands() {
            holding_hands.iter().map(|&p| &candidates[p]).find(|held| held.collides_with(c))
        } else {
            None
        };
        let disposition = match (blocker, c.degraded) {
            (Some(held), _) => CueDisposition::Suppressed { kind: c.kind, by: held.kind },
            (None, Some(why)) => CueDisposition::Degraded { kind: c.kind, why },
            (None, None) => CueDisposition::Played { kind: c.kind },
        };
        decided.push((c.index, disposition));
        if blocker.is_none() {
            scheduled.push(ScheduledAction { at: c.at, action: c.action.clone() });
            if c.reserves_hands() {
                holding_hands.push(position);
            }
        }
    }

    // Placement order was priority first, so `scheduled` is not yet in the order the page plays
    // it. Sorting here, once, is why `Claim` can reorder freely without the page ever seeing a cue
    // arrive before the one it follows.
    scheduled.extend(deliberate_nothings);
    scheduled.sort_by(|a, b| a.at.total_cmp(&b.at));

    decided.sort_by_key(|&(index, _)| index);
    (scheduled, decided.into_iter().map(|(_, d)| d).collect())
}
