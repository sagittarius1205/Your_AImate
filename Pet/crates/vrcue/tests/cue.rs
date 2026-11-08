//! `Cue` can only be built through its constructors, and a cue parsed from JSON goes through the
//! same constructors. Each test names one rule and fails when that rule is broken.

use serde_json::{Value, json};
use vrcue::{Cue, CueError, CueKind};

const NOT_A_TIME: [f32; 3] = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY];

fn parse(v: Value) -> Result<Cue, String> {
    serde_json::from_value::<Cue>(v).map_err(|e| e.to_string())
}

fn refused_with(v: Value, why: CueError) -> Result<(), String> {
    match parse(v.clone()) {
        Ok(cue) => Err(format!("{v} should be refused as `{why}`, but parsed as {cue:?}")),
        Err(msg) if msg.contains(&why.to_string()) => Ok(()),
        Err(msg) => Err(format!("{v} should be refused as `{why}`, but the error was `{msg}`")),
    }
}

#[test]
fn plain_refuses_the_two_kinds_that_carry_a_payload() {
    assert_eq!(Cue::plain(CueKind::Nod, 1.0), Err(CueError::NeedsPayload(CueKind::Nod)));
    assert_eq!(Cue::plain(CueKind::LookAway, 1.0), Err(CueError::NeedsPayload(CueKind::LookAway)));
}

#[test]
fn plain_builds_every_other_kind() {
    let others: Vec<CueKind> =
        CueKind::ALL.into_iter().filter(|k| !matches!(k, CueKind::Nod | CueKind::LookAway)).collect();
    assert_eq!(others.len(), 7);
    for kind in others {
        let cue = Cue::plain(kind, 1.5).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        assert_eq!((cue.kind(), cue.at()), (kind, 1.5));
    }
}

#[test]
fn every_constructor_refuses_a_time_that_is_not_finite() {
    for at in NOT_A_TIME {
        assert_eq!(Cue::plain(CueKind::Offer, at), Err(CueError::AtNotFinite), "plain at {at}");
        assert_eq!(Cue::nod(at, 3.0), Err(CueError::AtNotFinite), "nod at {at}");
        assert_eq!(Cue::look_away(at, 5.0, 1.0, 1.0), Err(CueError::AtNotFinite), "look_away at {at}");
    }
}

#[test]
fn every_constructor_refuses_a_negative_time_and_accepts_zero() {
    assert_eq!(Cue::plain(CueKind::Offer, -0.001), Err(CueError::AtNegative));
    assert_eq!(Cue::nod(-0.001, 3.0), Err(CueError::AtNegative));
    assert_eq!(Cue::look_away(-0.001, 1.0, 1.0, 1.0), Err(CueError::AtNegative));
    assert!(Cue::plain(CueKind::Offer, 0.0).is_ok());
    assert!(Cue::nod(0.0, 3.0).is_ok());
    assert!(Cue::look_away(0.0, 1.0, 1.0, 1.0).is_ok());
}

#[test]
fn nod_refuses_degrees_that_are_not_finite() {
    for degrees in NOT_A_TIME {
        assert_eq!(Cue::nod(1.0, degrees), Err(CueError::DegreesNotFinite), "degrees {degrees}");
    }
    assert!(Cue::nod(1.0, 7.0).is_ok());
}

#[test]
fn look_away_end_must_be_strictly_after_at() {
    assert_eq!(Cue::look_away(2.0, 2.0, 1.0, 1.0), Err(CueError::EndNotAfterAt));
    assert_eq!(Cue::look_away(2.0, 1.0, 1.0, 1.0), Err(CueError::EndNotAfterAt));
    assert!(Cue::look_away(2.0, 2.001, 1.0, 1.0).is_ok());
}

#[test]
fn look_away_refuses_an_end_yaw_or_pitch_that_is_not_finite() {
    for n in NOT_A_TIME {
        assert_eq!(Cue::look_away(1.0, n, 1.0, 1.0), Err(CueError::EndNotFinite), "end {n}");
        assert_eq!(Cue::look_away(1.0, 2.0, n, 1.0), Err(CueError::YawNotFinite), "yaw {n}");
        assert_eq!(Cue::look_away(1.0, 2.0, 1.0, n), Err(CueError::PitchNotFinite), "pitch {n}");
    }
    assert!(Cue::look_away(1.0, 2.0, -14.5, -8.0).is_ok(), "yaw and pitch may be negative");
}

#[test]
fn json_shape_is_flat_and_every_shape_reads_back_equal() {
    let cases: [(Cue, Value); 3] = [
        (Cue::plain(CueKind::BeatBoth, 1.25).unwrap(), json!({ "kind": "beatBoth", "at": 1.25 })),
        (Cue::nod(1.5, 7.0).unwrap(), json!({ "kind": "nod", "at": 1.5, "degrees": 7.0 })),
        (
            Cue::look_away(2.0, 3.5, -12.5, -8.0).unwrap(),
            json!({ "kind": "lookAway", "at": 2.0, "end": 3.5, "yaw": -12.5, "pitch": -8.0 }),
        ),
    ];
    for (cue, wire) in cases {
        assert_eq!(serde_json::to_value(cue).unwrap(), wire, "serialized form of {cue:?}");
        assert_eq!(parse(wire.clone()), Ok(cue), "parsing {wire}");
    }
}

#[test]
fn json_cue_goes_through_the_same_gate_as_the_constructors() {
    refused_with(json!({ "kind": "offer", "at": -1.0 }), CueError::AtNegative).unwrap();
    refused_with(json!({ "kind": "nod", "at": 1.0, "degrees": 1e40 }), CueError::DegreesNotFinite).unwrap();
    refused_with(json!({ "kind": "offer", "at": 1e40 }), CueError::AtNotFinite).unwrap();
    refused_with(
        json!({ "kind": "lookAway", "at": 2.0, "end": 2.0, "yaw": 1.0, "pitch": 1.0 }),
        CueError::EndNotAfterAt,
    )
    .unwrap();
}

#[test]
fn json_refuses_a_cue_that_lacks_a_field_its_kind_requires() {
    let missing = |kind, field| CueError::PayloadMissing { kind, field };
    refused_with(json!({ "kind": "nod", "at": 1.0 }), missing(CueKind::Nod, "degrees")).unwrap();
    refused_with(
        json!({ "kind": "lookAway", "at": 1.0, "end": 2.0, "yaw": 1.0 }),
        missing(CueKind::LookAway, "pitch"),
    )
    .unwrap();
    refused_with(json!({ "kind": "lookAway", "at": 1.0 }), missing(CueKind::LookAway, "end")).unwrap();
}

#[test]
fn json_refuses_a_cue_that_carries_a_field_of_another_kind() {
    let foreign = |kind, field| CueError::PayloadUnexpected { kind, field };
    refused_with(json!({ "kind": "nod", "at": 1.0, "degrees": 3.0, "yaw": 5.0 }), foreign(CueKind::Nod, "yaw")).unwrap();
    refused_with(json!({ "kind": "offer", "at": 1.0, "degrees": 3.0 }), foreign(CueKind::Offer, "degrees")).unwrap();
    refused_with(json!({ "kind": "rest", "at": 1.0, "end": 2.0 }), foreign(CueKind::Rest, "end")).unwrap();
    refused_with(
        json!({ "kind": "lookAway", "at": 1.0, "end": 2.0, "yaw": 1.0, "pitch": 1.0, "degrees": 3.0 }),
        foreign(CueKind::LookAway, "degrees"),
    )
    .unwrap();
}

#[test]
fn json_refuses_a_field_it_does_not_know() {
    let msg = parse(json!({ "kind": "offer", "at": 1.0, "because": "you" })).unwrap_err();
    assert!(msg.contains("unknown field `because`"), "{msg}");
}
