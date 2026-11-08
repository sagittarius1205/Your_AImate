//! What the body does while a reply is said: the second half of `VRGesturePlanner::Plan`
//! (Source/VRPhysProto/VRGesturePlan.cpp:302-440), ported, then scheduled and resolved for the model
//! the page shows.
//!
//! The cues come from three places, as in the C++. The `[tags]` the core put in front of its clauses
//! (`self`, `offer`, `open`, `question`, `nod`, `think`); a clause that ends in a question mark; and
//! the loudest syllables in the voice, which get beats. After the hands are placed, beats take turns
//! between one hand and both, every second one nods a little, and between sentences the eyes go
//! elsewhere while the next one is found.
//!
//! **Not ported: the 24 keyword rules** (VRGesturePlan.cpp:101-126, 320-353). They exist for text
//! nobody tagged, and every reply the pet says comes from the core, which tags each clause itself
//! (ChatCore/src/reply.rs). A second rule engine here would put gestures on words the model chose
//! not to gesture on.
//!
//! **Not ported: the brows** (VRGesturePlan.cpp:416, 421). Mao's brows sit under its fringe: lifting
//! both from 0.3 to 1.0 changes 311 down to 288 pixels of the 1200x1520 canvas, against 787 for a
//! 0.3 glance of the eyes and 40565 for a beat (measured 2026-11-05, Docs/DESIGN_Pet.md 4.1.1).
//!
//! The overlap rule is not ported a second time: `resolve::schedule` is it, and this file calls it
//! twice. Once to learn which beats keep the hands, because the C++ alternates and nods on the beats
//! that survive (:404-424), and once on everything, for the actions and dispositions the page gets.
//!
//! Randomness is seeded from the text (`seed`), so the same reply always gets the same plan. The
//! generator is UE's `FRandomStream`, bit for bit, so a seed means the same draws on both sides.

use serde::Serialize;
use vrcue::{Bindings, Cue, CueDisposition, CueKind, Measure};

use super::align::{Alignment, FRAME_SECONDS, align, ends_sentence};
use super::resolve::{Claim, ScheduledAction, schedule};

/// VRGesturePlan.cpp:146. A syllable stands out when it is at least this share of the loudest frame
const BEAT_SHARE: f32 = 0.35;
/// VRGesturePlan.cpp:147. and the loudest within this many frames either side.
const BEAT_NEIGHBOURS: usize = 5;
/// Seconds from the end of one glance to the start of the next, at the least. The head is back
/// 0.55 s after a glance ends and starts the next 0.15 s before it (cue/figure.json), so 0.8 s leaves
/// the head still for a moment in between. Not in the C++ (`keep_glances_apart`).
const GLANCES_APART: f32 = 0.8;
/// Two glances closer than this go the same way, so the head never swings across in between.
const SAME_SIDE_WITHIN: f32 = 1.5;

/// UE's `FRandomStream` (Engine/Source/Runtime/Core/Public/Math/RandomStream.h in 5.8), the three
/// calls the planner makes.
struct RandomStream(u32);

impl RandomStream {
    /// `GetFraction`: a float in [0, 1) made from the top 23 bits of the next seed.
    fn frand(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(196_314_165).wrapping_add(907_633_515);
        f32::from_bits(0x3F80_0000 | (self.0 >> 9)) - 1.0
    }

    /// `FRandRange` computes in `FVector::FReal`, which is double in UE 5; the planner casts back.
    fn frand_range(&mut self, min: f32, max: f32) -> f32 {
        (f64::from(min) + (f64::from(max) - f64::from(min)) * f64::from(self.frand())) as f32
    }

    fn sign(&mut self) -> f32 {
        if self.frand() < 0.5 { -1.0 } else { 1.0 }
    }
}

/// The seed for a reply: FNV-1a over its UTF-8 bytes, so the same text always plans the same.
pub fn seed(text: &str) -> u32 {
    text.bytes().fold(0x811C_9DC5u32, |hash, b| (hash ^ u32::from(b)).wrapping_mul(0x0100_0193))
}

/// What the page is handed for one voiced reply. Actions in stroke order, already resolved and
/// already clear of each other; one disposition per planned cue, so a cue that did not play is a
/// number and not an absence.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Plan {
    pub actions: Vec<ScheduledAction>,
    pub dispositions: Vec<CueDisposition>,
}

/// A planned time as a cue time. A stroke the planner puts before the first sample (a `[think]` on
/// the first character glances from 0.1 s before it, VRGesturePlan.cpp:313) starts at the clip's
/// start instead: the rise before it is already off screen, and refusing the cue would lose it.
fn clip_time(t: f32) -> f32 {
    t.max(0.0)
}

fn gesture(kind: CueKind, at: f32) -> Cue {
    Cue::plain(kind, clip_time(at)).expect("a planned gesture is a plain kind at a finite time")
}

fn nod(at: f32, degrees: f32) -> Cue {
    Cue::nod(clip_time(at), degrees).expect("a planned nod has a finite time and size")
}

fn glance(start: f32, end: f32, yaw: f32, pitch: f32) -> Cue {
    Cue::look_away(clip_time(start), end, yaw, pitch).expect("a planned glance ends after it starts")
}

/// The hand candidates, in the order the C++ creates them (tags, then question marks, then beats),
/// because input position breaks ties in placement as the C++ `StableSort` does. Nods and the
/// `[think]` glances found on the way are returned beside them, with the random draws made in the
/// C++ order.
fn candidates(a: &Alignment, random: &mut RandomStream) -> (Vec<(Cue, Claim)>, Vec<Cue>, Vec<Cue>) {
    let mut hands = Vec::new();
    let mut nods = Vec::new();
    let mut gazes = Vec::new();

    // Tags (VRGesturePlan.cpp:304-319). The tag table is vrcue's, which the core is pinned to.
    for tag in &a.tags {
        let at = a.char_time[tag.at.min(a.said.len())];
        match CueKind::ALL.into_iter().find(|k| k.tags().contains(&tag.name.as_str())) {
            Some(CueKind::Nod) => nods.push(nod(at + 0.15, 7.0)),
            Some(CueKind::LookAway) => {
                let yaw = random.frand_range(14.0, 20.0) * random.sign();
                let pitch = random.frand_range(5.0, 10.0);
                gazes.push(glance(at - 0.1, at + 1.0, yaw, pitch));
            }
            Some(kind) => hands.push((gesture(kind, at + 0.1), Claim::Tag)),
            None => {}
        }
    }

    // A clause that asks something ends with the hands asking too (VRGesturePlan.cpp:354-361).
    for (k, &(_, end)) in a.spans.iter().enumerate() {
        if matches!(a.said[end - 1], '？' | '?') {
            hands.push((gesture(CueKind::Question, a.clause_times[k].1 - 0.2), Claim::QuestionMark));
        }
    }

    // Beats on the syllables that stand out, inside the clauses (VRGesturePlan.cpp:362-377).
    let loud = &a.loudness;
    for &(from, to) in &a.clause_times {
        let first = (from / FRAME_SECONDS).floor() as usize;
        let past = loud.len().min((to / FRAME_SECONDS).ceil() as usize);
        for f in first..past {
            let near = f.saturating_sub(BEAT_NEIGHBOURS)..=(loud.len() - 1).min(f + BEAT_NEIGHBOURS);
            if loud[f] >= a.peak * BEAT_SHARE && near.into_iter().all(|n| loud[n] <= loud[f]) {
                hands.push((gesture(CueKind::Beat, f as f32 * FRAME_SECONDS), Claim::Stress(loud[f])));
            }
        }
    }
    (hands, nods, gazes)
}

/// The glances that are kept, in time order: the `[think]` ones first, then the pauses' in the room
/// left, each at least `GLANCES_APART` from every glance kept before it, and on the same side as any
/// kept glance within `SAME_SIDE_WITHIN`.
///
/// The C++ has no such rule. Nothing there keeps a `[think]` glance and a pause glance apart, and in
/// the pet's first filmed reply the head went 13 degrees one way and 17 the other 0.46 s later, two
/// glances overlapping and pulling against each other. One head can look one way at a time, and a
/// head that swings back across within a second reads as a twitch. The words asked for the `[think]`
/// glance, so it is placed first, as a tag claims the hands before a beat (`resolve::Claim`). A pause
/// glance that would crowd it is not made at all, which is what the C++ does with a pause too short
/// for one (:432), so it has no disposition.
fn keep_glances_apart(tagged: Vec<Cue>, pauses: Vec<Cue>) -> Vec<Cue> {
    let window = |g: &Cue| (g.at(), g.end().unwrap_or(g.at()));
    let gap = |x: &Cue, y: &Cue| {
        let ((xs, xe), (ys, ye)) = (window(x), window(y));
        (ys - xe).max(xs - ye)
    };
    let mut kept: Vec<Cue> = Vec::new();
    for g in tagged.into_iter().chain(pauses) {
        if kept.iter().any(|k| gap(k, &g) < GLANCES_APART) {
            continue;
        }
        let yaw = g.measure(Measure::Yaw).unwrap_or(0.0);
        let nearest = kept.iter().filter(|k| gap(k, &g) < SAME_SIDE_WITHIN).min_by(|x, y| gap(x, &g).total_cmp(&gap(y, &g)));
        let g = match nearest.and_then(|k| k.measure(Measure::Yaw)) {
            Some(theirs) if theirs * yaw < 0.0 => {
                let (start, end) = window(&g);
                glance(start, end, -yaw, g.measure(Measure::Pitch).unwrap_or(0.0))
            }
            _ => g,
        };
        kept.push(g);
    }
    kept.sort_by(|x, y| x.at().total_cmp(&y.at()));
    kept
}

/// The planned cues, in the order their dispositions come back: hands, then nods, then glances.
pub fn cues(text: &str, samples: &[f32], sample_rate: u32, bindings: &Bindings) -> Vec<(Cue, Claim)> {
    let Some(a) = align(text, samples, sample_rate) else {
        return Vec::new();
    };
    let mut random = RandomStream(seed(text));
    let (mut hands, mut nods, gazes) = candidates(&a, &mut random);

    // Which hand cues keep the hands, by the one overlap rule there is.
    let (_, placed) = schedule(&hands, bindings);
    let mut kept: Vec<usize> =
        (0..hands.len()).filter(|&i| matches!(placed[i], CueDisposition::Played { .. } | CueDisposition::Degraded { .. })).collect();
    kept.sort_by(|&x, &y| hands[x].0.at().total_cmp(&hands[y].0.at()));

    // Beats take turns between one hand and both, and every second one nods 3 degrees unless a nod
    // is already within 0.8 s (VRGesturePlan.cpp:404-418).
    let mut beats = 0;
    for i in kept {
        let (cue, claim) = hands[i];
        if cue.kind() != CueKind::Beat {
            continue;
        }
        if beats % 2 == 1 {
            hands[i] = (gesture(CueKind::BeatBoth, cue.at()), claim);
            if !nods.iter().any(|n| (n.at() - cue.at()).abs() < 0.8) {
                nods.push(nod(cue.at(), 3.0));
            }
        }
        beats += 1;
    }
    nods.sort_by(|x, y| x.at().total_cmp(&y.at()));

    // Between sentences the eyes go down and to the side while the next one is found, and come
    // back as it starts, the side alternating (VRGesturePlan.cpp:426-439).
    let mut side = random.sign();
    let mut pauses = Vec::new();
    for k in 0..a.spans.len().saturating_sub(1) {
        let (pause_start, pause_end) = (a.clause_times[k].1, a.clause_times[k + 1].0);
        if ends_sentence(a.said[a.spans[k].1 - 1]) && pause_end - pause_start >= 0.2 {
            let yaw = side * random.frand_range(10.0, 16.0);
            let pitch = -random.frand_range(6.0, 14.0);
            pauses.push(glance(pause_start - 0.05, pause_end + 0.35, yaw, pitch));
            side = -side;
        }
    }
    let gazes = keep_glances_apart(gazes, pauses);

    // Nods and glances never contend for anything, so their claim decides nothing.
    hands.into_iter().chain(nods.into_iter().chain(gazes).map(|cue| (cue, Claim::Tag))).collect()
}

/// The plan for one voiced reply.
pub fn plan(text: &str, samples: &[f32], sample_rate: u32, bindings: &Bindings) -> Plan {
    let (actions, dispositions) = schedule(&cues(text, samples, sample_rate, bindings), bindings);
    Plan { actions, dispositions }
}
