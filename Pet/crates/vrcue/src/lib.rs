//! The pet's closed cue vocabulary, the table that binds each cue to something a character can do,
//! and the one function that turns the first into the second.
//!
//! A leaf crate. The pet backend and the conversation core both depend on it by path, so there is
//! one list of cues and no copy to drift (Docs/DESIGN_Pet.md section 2).

use std::fmt;
use std::ops::Index;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---------------------------------------------------------------------------------------------
// Vocabulary

/// `EVRGesture` without `None` (which means "no cue"), plus the two non-hand channels.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CueKind {
    /// `EVRGesture::Self`. `Self` is a reserved word in Rust.
    SelfRef,
    Offer,
    Open,
    Question,
    Beat,
    BeatBoth,
    Rest,
    /// `FVRNodCue`.
    Nod,
    /// `FVRGazeCue`.
    LookAway,
}

impl CueKind {
    pub const ALL: [CueKind; 9] = [
        CueKind::SelfRef,
        CueKind::Offer,
        CueKind::Open,
        CueKind::Question,
        CueKind::Beat,
        CueKind::BeatBoth,
        CueKind::Rest,
        CueKind::Nod,
        CueKind::LookAway,
    ];

    /// Position in [`CueKind::ALL`]. `pet_vocab` asserts the two agree.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Where the pet gets this cue from. Docs/DESIGN_Pet.md 2.1: this decides the division of
    /// labour of the whole pipeline, so it is part of the design and not a helper.
    pub const fn origin(self) -> Origin {
        match self {
            CueKind::SelfRef
            | CueKind::Offer
            | CueKind::Open
            | CueKind::Question
            | CueKind::Nod
            | CueKind::LookAway => Origin::Semantic,
            CueKind::Beat | CueKind::BeatBoth => Origin::Prosodic,
            CueKind::Rest => Origin::Baseline,
        }
    }

    /// The spellings `VRGesturePlanner` accepts (VRGesturePlan.cpp:128-135 for the four hand
    /// gestures, :307 and :311 for `nod` and `think`). Beat, BeatBoth and Rest have no tag: audio
    /// and the gap between gestures produce them.
    pub const fn tags(self) -> &'static [&'static str] {
        match self {
            CueKind::SelfRef => &["self"],
            CueKind::Offer => &["you", "offer"],
            CueKind::Open => &["open"],
            CueKind::Question => &["shrug", "question"],
            CueKind::Nod => &["nod"],
            CueKind::LookAway => &["think"],
            CueKind::Beat | CueKind::BeatBoth | CueKind::Rest => &[],
        }
    }

    /// Rise, hold and fall around the stroke, in seconds. Every number is copied from the C++
    /// source, which is the authority. A kind with no number there has an all-zero span and a
    /// named TODO, not an invented duration.
    pub const fn span(self) -> Span {
        match self {
            CueKind::SelfRef | CueKind::Offer | CueKind::Open | CueKind::Question => HAND_GESTURE_SPAN,
            CueKind::Beat | CueKind::BeatBoth => BEAT_SPAN,
            CueKind::Nod => NOD_SPAN,
            // C++ has no rise/hold/fall for Rest. SpanOf (VRGesturePlan.cpp:20-23) only reaches
            // Rest by falling through its ternary, and no call site passes it: the rest pose is an
            // exponential approach (VRBodyLanguageComponent.cpp:298, RestSpeed 3.0 at :11), not an
            // event. Nothing plans a Rest cue, and a model whose idle motion is its rest binds it to
            // `Nothing` (Mao does), so no duration is needed and none is invented.
            CueKind::Rest => NO_SPAN,
            // A glance carries its own window, `at` to `end` (VRGesturePlan.cpp:313, :436), so its
            // length is the cue's and not the kind's. `resolve` stretches each lane's hold over that
            // window. Zero here only says the kind has no fixed length, and the Eyes channel never
            // takes part in overlap, which is the one place this span would be read.
            CueKind::LookAway => NO_SPAN,
        }
    }

    /// Which part of the body this cue moves.
    ///
    /// Added during implementation; Docs/DESIGN_Pet.md 2.1 has no such function. It exists so that
    /// "two gestures never overlap" and "Nod and LookAway take no part in overlap" are one rule
    /// ([`Channel::exclusive`]) and not two special cases in the scheduler.
    pub const fn occupies(self) -> Channel {
        match self {
            CueKind::SelfRef
            | CueKind::Offer
            | CueKind::Open
            | CueKind::Question
            | CueKind::Beat
            | CueKind::BeatBoth => Channel::Hands,
            CueKind::Nod => Channel::Head,
            CueKind::LookAway => Channel::Eyes,
            CueKind::Rest => Channel::Baseline,
        }
    }
}

/// VRGesturePlan.cpp:22, second branch of `SpanOf`: every gesture that is not a beat.
const HAND_GESTURE_SPAN: Span = Span { rise: 0.45, hold: 0.25, fall: 0.75 };
/// VRGesturePlan.cpp:22, first branch of `SpanOf`: a beat is a quick flick.
const BEAT_SPAN: Span = Span { rise: 0.3, hold: 0.08, fall: 0.5 };
/// Derived, not copied. A nod is a raised cosine over `NodHalf` either side of its deepest point
/// (VRBodyLanguageComponent.cpp:28, used at :201-203), which reads as a rise, no hold, and a fall.
const NOD_SPAN: Span = Span { rise: 0.3, hold: 0.0, fall: 0.3 };
const NO_SPAN: Span = Span { rise: 0.0, hold: 0.0, fall: 0.0 };

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Origin {
    /// The core declares it from a tag and it arrives on the stream: SelfRef, Offer, Open,
    /// Question, Nod, LookAway.
    Semantic,
    /// Computed from audio loudness by the pet itself: Beat, BeatBoth.
    Prosodic,
    /// A state between gestures, not an event: Rest.
    Baseline,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Channel {
    Hands,
    Head,
    Eyes,
    Baseline,
}

impl Channel {
    /// Two cues on an exclusive channel cannot be on screen together. Only the hands are: a second
    /// gesture that starts before the first has landed looks like twitching (VRGesturePlan.cpp:388),
    /// while a nod during a gesture, or a glance during a nod, is just a person.
    pub const fn exclusive(self) -> bool {
        matches!(self, Channel::Hands)
    }
}

/// `FVRGestureSpan` (VRGesturePlan.h:24): `rise` seconds getting to the stroke, `hold` at it, and
/// gone `fall` seconds after it.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Span {
    pub rise: f32,
    pub hold: f32,
    pub fall: f32,
}

// ---------------------------------------------------------------------------------------------
// Cue

/// A cue the pet received or computed. `at` and `end` are seconds from the start of the utterance.
///
/// The fields are private and there are three constructors, so a Nod cannot carry gaze angles and a
/// cue cannot hold a time that is negative or not a number. A cue that arrives as JSON from another
/// process is parsed through the same constructors.
#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(try_from = "CueWire")]
pub struct Cue {
    kind: CueKind,
    at: f32,
    #[serde(flatten)]
    detail: CueDetail,
}

#[derive(Copy, Clone, PartialEq, Debug, Serialize)]
#[serde(untagged)]
enum CueDetail {
    Plain,
    Nod { degrees: f32 },
    Gaze { end: f32, yaw: f32, pitch: f32 },
}

/// The reason a cue was refused. One variant per reason, so a caller can name it.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum CueError {
    AtNotFinite,
    AtNegative,
    EndNotFinite,
    EndNotAfterAt,
    DegreesNotFinite,
    YawNotFinite,
    PitchNotFinite,
    /// `Cue::plain` was given a kind that has a payload.
    NeedsPayload(CueKind),
    /// A wire cue of this kind lacks a field that the kind requires.
    PayloadMissing { kind: CueKind, field: &'static str },
    /// A wire cue of this kind carries a field that belongs to another kind.
    PayloadUnexpected { kind: CueKind, field: &'static str },
}

impl fmt::Display for CueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CueError::AtNotFinite => f.write_str("cue time `at` is not a finite number"),
            CueError::AtNegative => f.write_str("cue time `at` is negative"),
            CueError::EndNotFinite => f.write_str("gaze `end` is not a finite number"),
            CueError::EndNotAfterAt => f.write_str("gaze `end` is not after `at`"),
            CueError::DegreesNotFinite => f.write_str("nod `degrees` is not a finite number"),
            CueError::YawNotFinite => f.write_str("gaze `yaw` is not a finite number"),
            CueError::PitchNotFinite => f.write_str("gaze `pitch` is not a finite number"),
            CueError::NeedsPayload(kind) => {
                write!(f, "{kind:?} carries a payload, so it cannot be built with Cue::plain")
            }
            CueError::PayloadMissing { kind, field } => write!(f, "{kind:?} cue is missing `{field}`"),
            CueError::PayloadUnexpected { kind, field } => {
                write!(f, "{kind:?} cue must not carry `{field}`")
            }
        }
    }
}

impl std::error::Error for CueError {}

fn check_at(at: f32) -> Result<(), CueError> {
    if !at.is_finite() {
        Err(CueError::AtNotFinite)
    } else if at < 0.0 {
        Err(CueError::AtNegative)
    } else {
        Ok(())
    }
}

impl Cue {
    /// Every kind except Nod and LookAway, which carry numbers and have their own constructors.
    pub fn plain(kind: CueKind, at: f32) -> Result<Cue, CueError> {
        if matches!(kind, CueKind::Nod | CueKind::LookAway) {
            return Err(CueError::NeedsPayload(kind));
        }
        check_at(at)?;
        Ok(Cue { kind, at, detail: CueDetail::Plain })
    }

    pub fn nod(at: f32, degrees: f32) -> Result<Cue, CueError> {
        check_at(at)?;
        if !degrees.is_finite() {
            return Err(CueError::DegreesNotFinite);
        }
        Ok(Cue { kind: CueKind::Nod, at, detail: CueDetail::Nod { degrees } })
    }

    /// Eyes off the listener from `at` to `end`, by `yaw` and `pitch` degrees.
    pub fn look_away(at: f32, end: f32, yaw: f32, pitch: f32) -> Result<Cue, CueError> {
        check_at(at)?;
        if !end.is_finite() {
            return Err(CueError::EndNotFinite);
        }
        if end <= at {
            return Err(CueError::EndNotAfterAt);
        }
        if !yaw.is_finite() {
            return Err(CueError::YawNotFinite);
        }
        if !pitch.is_finite() {
            return Err(CueError::PitchNotFinite);
        }
        Ok(Cue { kind: CueKind::LookAway, at, detail: CueDetail::Gaze { end, yaw, pitch } })
    }

    pub fn kind(&self) -> CueKind {
        self.kind
    }

    pub fn at(&self) -> f32 {
        self.at
    }

    /// The number this cue carries for `measure`, or `None` when its kind carries no such number.
    pub fn measure(&self, measure: Measure) -> Option<f32> {
        match (self.detail, measure) {
            (CueDetail::Nod { degrees }, Measure::Degrees) => Some(degrees),
            (CueDetail::Gaze { yaw, .. }, Measure::Yaw) => Some(yaw),
            (CueDetail::Gaze { pitch, .. }, Measure::Pitch) => Some(pitch),
            _ => None,
        }
    }

    /// Where a cue that carries its own window ends. Only a glance does.
    pub fn end(&self) -> Option<f32> {
        match self.detail {
            CueDetail::Gaze { end, .. } => Some(end),
            _ => None,
        }
    }
}

/// One of the numbers a cue carries, which a lane can be sized by.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Measure {
    /// How far a nod dips (`FVRNodCue::Degrees`).
    Degrees,
    /// How far a glance turns aside, signed (`FVRGazeCue::Yaw`).
    Yaw,
    /// How far a glance turns up, signed (`FVRGazeCue::Pitch`).
    Pitch,
}

impl Measure {
    /// The kinds whose cues carry this number. A lane sized by a number its kind does not carry
    /// would always come out at zero, so a binding profile is checked against this.
    pub const fn carried_by(self, kind: CueKind) -> bool {
        matches!(
            (self, kind),
            (Measure::Degrees, CueKind::Nod) | (Measure::Yaw | Measure::Pitch, CueKind::LookAway)
        )
    }
}

/// The JSON shape of a cue: flat, with the payload fields optional. Only `Cue::try_from` reads it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CueWire {
    kind: CueKind,
    at: f32,
    degrees: Option<f32>,
    end: Option<f32>,
    yaw: Option<f32>,
    pitch: Option<f32>,
}

impl TryFrom<CueWire> for Cue {
    type Error = CueError;

    fn try_from(w: CueWire) -> Result<Cue, CueError> {
        let kind = w.kind;
        let fields = [("degrees", w.degrees), ("end", w.end), ("yaw", w.yaw), ("pitch", w.pitch)];
        let allowed: &[&str] = match kind {
            CueKind::Nod => &["degrees"],
            CueKind::LookAway => &["end", "yaw", "pitch"],
            _ => &[],
        };
        if let Some(&(field, _)) = fields.iter().find(|(name, v)| v.is_some() && !allowed.contains(name)) {
            return Err(CueError::PayloadUnexpected { kind, field });
        }
        let need = |field: &'static str| {
            fields
                .iter()
                .find(|(name, _)| *name == field)
                .and_then(|&(_, v)| v)
                .ok_or(CueError::PayloadMissing { kind, field })
        };
        match kind {
            CueKind::Nod => Cue::nod(w.at, need("degrees")?),
            CueKind::LookAway => Cue::look_away(w.at, need("end")?, need("yaw")?, need("pitch")?),
            _ => Cue::plain(kind, w.at),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What the page can be told to do

/// The three things the page knows how to do. No cue name appears on it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CharacterAction {
    /// Play a motion file. `group` and `index` were checked against the loaded model3.json.
    PlayMotion { group: String, index: u32, fade_in: f32 },
    /// Push a set of standard parameters to a target and let them go. Head, eyes and brows.
    DriveParams { lanes: Vec<ParamLane> },
    /// Do nothing, on purpose.
    Nothing,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ParamLane {
    /// A parameter id of the model, such as "ParamAngleY".
    pub id: String,
    /// In that parameter's own unit; the page converts nothing. In a binding with `per` set, this
    /// is the value for one unit of that cue number, and `resolve` multiplies it out, so an action
    /// the page receives never has `per`.
    pub to: f32,
    /// The cue number this lane's size follows, so a 3 degree nod and a 7 degree nod are not the
    /// same nod, and a glance goes the side and the height the cue says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per: Option<Measure>,
    /// `FVRGestureSpan` as it is, in seconds: getting there before the stroke, held after it, and
    /// gone by `fall` after it (`fall` counts from the stroke, VRBodyLanguageComponent.cpp:120-129).
    /// A cue with its own window stretches `hold` over the window and keeps the two ramps.
    pub rise: f32,
    pub hold: f32,
    pub fall: f32,
    /// How long after the cue's stroke this lane's own stroke lands, in seconds; the ramps move
    /// with it. Rise, hold and fall put every lane at full at the same instant, so an arm whose
    /// joints all share them starts and stops as one piece. A shoulder at 0, an elbow a little
    /// later and the wrist later still is an arm that moves as a chain and settles last at the hand.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub lag: f32,
    /// `to` is added to what the idle motion gave the parameter instead of replacing it. For a
    /// parameter the idle keeps moving, such as Mao's body, which its idle turns from -6 to 3: an
    /// absolute target there leans one way or the other depending on where the idle happened to be,
    /// while an offset leans the same way every time.
    #[serde(default, skip_serializing_if = "is_false")]
    pub relative: bool,
}

fn is_zero(x: &f32) -> bool {
    *x == 0.0
}

fn is_false(x: &bool) -> bool {
    !*x
}

// ---------------------------------------------------------------------------------------------
// Bindings

/// What a cue is bound to, per kind. There is no "this key is absent" state: every kind has a slot,
/// so a label cannot quietly fall through.
#[derive(Clone, PartialEq, Debug)]
pub struct Bindings([Binding; CueKind::ALL.len()]);

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Binding {
    /// Guessed from a naming convention. The pattern stays so the binding UI can answer "why this
    /// one". An owned string, unlike the design sketch's `&'static str`, because this is persisted
    /// in the settings file and a borrowed `'static` cannot be read back from it.
    Guessed { action: CharacterAction, pattern: String },
    /// Clicked once by the user, or matched exactly by an authored profile.
    Chosen { action: CharacterAction, evidence: Evidence },
    /// Something plays, but not what this cue should be, and the substitution is stated. A Beat
    /// played for a BeatBoth is a degradation; it must not pass for a two-handed motion.
    Degraded { action: CharacterAction, why: Degradation },
    /// The user said this model has no such motion. That is a decision.
    DeclaredAbsent,
    /// Nobody has decided. The only state the UI nags about.
    Unbound,
}

/// A guessed file name is not evidence.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Evidence {
    AuthoredProfile,
    UserPreviewed,
    ExactFingerprintProfile,
}

/// Why a binding is not a faithful rendering of its cue. The design names the idea (DESIGN_Pet.md
/// 2.2) but not the type; this has the one case it gives and grows as bindings need more.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Degradation {
    /// The motion that belongs to another cue kind plays in this kind's place.
    PlaysAs(CueKind),
}

impl Binding {
    pub fn degradation(&self) -> Option<Degradation> {
        match self {
            Binding::Degraded { why, .. } => Some(*why),
            _ => None,
        }
    }
}

impl Default for Bindings {
    /// Every slot `Unbound`: nothing is decided until somebody decides it.
    fn default() -> Bindings {
        Bindings(std::array::from_fn(|_| Binding::Unbound))
    }
}

impl Index<CueKind> for Bindings {
    type Output = Binding;

    fn index(&self, kind: CueKind) -> &Binding {
        &self.0[kind.index()]
    }
}

impl Bindings {
    pub fn set(&mut self, kind: CueKind, binding: Binding) {
        self.0[kind.index()] = binding;
    }

    pub fn iter(&self) -> impl Iterator<Item = (CueKind, &Binding)> {
        CueKind::ALL.into_iter().zip(self.0.iter())
    }
}

/// Written as an object with all nine keys.
impl Serialize for Bindings {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

/// Read as an object keyed by cue kind. A missing key becomes `Unbound`, never a dropped slot, so a
/// settings file written before a kind existed reads back with that kind visibly undecided
/// (Docs/DESIGN_Pet.md section 7). A key that is not a cue kind, or appears twice, is an error: a
/// binding must not be discarded without a word.
impl<'de> Deserialize<'de> for Bindings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Bindings, D::Error> {
        struct Slots;

        impl<'de> Visitor<'de> for Slots {
            type Value = Bindings;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object keyed by cue kind")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Bindings, A::Error> {
                let mut slots: [Option<Binding>; CueKind::ALL.len()] = Default::default();
                while let Some(kind) = map.next_key::<CueKind>()? {
                    let slot = &mut slots[kind.index()];
                    if slot.is_some() {
                        return Err(de::Error::custom(format!("duplicate binding for cue kind {kind:?}")));
                    }
                    *slot = Some(map.next_value()?);
                }
                Ok(Bindings(slots.map(|slot| slot.unwrap_or(Binding::Unbound))))
            }
        }

        deserializer.deserialize_map(Slots)
    }
}

// ---------------------------------------------------------------------------------------------
// Resolution

/// Why a cue has no action.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Unresolved {
    /// Nobody has decided what this model does for the cue.
    Unbound,
}

/// The only place a cue becomes an action.
///
/// `Unbound` is `Err` and `DeclaredAbsent` is `Ok(Nothing)`, and that difference is the whole point
/// of this function. "Nobody decided" and "decided to do nothing" are two different things in the
/// product; folding them into one value is the silent failure this type exists to prevent.
///
/// The binding says what moves; the cue says how far and for how long. A lane with `per` is
/// multiplied by that number of the cue, and a cue with its own window holds every lane for the
/// length of it (Docs/DESIGN_Pet.md section 11, items 10 and 11).
pub fn resolve(cue: &Cue, bindings: &Bindings) -> Result<CharacterAction, Unresolved> {
    match &bindings[cue.kind()] {
        Binding::Guessed { action, .. } | Binding::Chosen { action, .. } | Binding::Degraded { action, .. } => {
            Ok(fit(action, cue))
        }
        Binding::DeclaredAbsent => Ok(CharacterAction::Nothing),
        Binding::Unbound => Err(Unresolved::Unbound),
    }
}

/// `action` sized and timed for this one cue.
fn fit(action: &CharacterAction, cue: &Cue) -> CharacterAction {
    let CharacterAction::DriveParams { lanes } = action else {
        return action.clone();
    };
    let lanes = lanes
        .iter()
        .map(|lane| {
            // A number the cue does not carry sizes the lane to zero: nothing moves, and the
            // profile check (`Measure::carried_by`) keeps an authored binding from asking.
            let to = match lane.per {
                Some(measure) => lane.to * cue.measure(measure).unwrap_or(0.0),
                None => lane.to,
            };
            let (hold, fall) = match cue.end() {
                Some(end) => {
                    let hold = end - cue.at();
                    (hold, hold + (lane.fall - lane.hold))
                }
                None => (lane.hold, lane.fall),
            };
            ParamLane { id: lane.id.clone(), to, per: None, rise: lane.rise, hold, fall, lag: lane.lag, relative: lane.relative }
        })
        .collect();
    CharacterAction::DriveParams { lanes }
}

/// What happened to one cue this time. The four failure variants are four different problems, and
/// merging any two of them hides which one it is: eaten by the overlap rule, substituted, declared
/// impossible for this model, or never decided at all.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize)]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum CueDisposition {
    Played { kind: CueKind },
    Degraded { kind: CueKind, why: Degradation },
    /// The user said this model has no such motion, so nothing plays on purpose.
    Unavailable { kind: CueKind },
    /// Nobody has bound this cue. Separate from `Unavailable` for the same reason `resolve` returns
    /// `Err` for `Unbound` and `Ok(Nothing)` for `DeclaredAbsent`: a missing decision is the binding
    /// UI's job to nag about, a declared absence is already answered, and `pet_coverage` has to be
    /// able to tell a model that lacks an arm from a model nobody finished binding.
    Unresolved { kind: CueKind },
    Suppressed { kind: CueKind, by: CueKind },
}

impl CueDisposition {
    pub fn kind(&self) -> CueKind {
        match *self {
            CueDisposition::Played { kind }
            | CueDisposition::Degraded { kind, .. }
            | CueDisposition::Unavailable { kind }
            | CueDisposition::Unresolved { kind }
            | CueDisposition::Suppressed { kind, .. } => kind,
        }
    }
}
