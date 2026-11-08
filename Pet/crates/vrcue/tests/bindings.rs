//! `resolve` and the `Bindings` table, as a caller sees them.

use serde_json::{Value, json};
use vrcue::{
    Binding, Bindings, CharacterAction, Cue, CueKind, Degradation, Evidence, Unresolved, resolve,
};

fn motion(group: &str) -> CharacterAction {
    CharacterAction::PlayMotion { group: group.to_string(), index: 0, fade_in: 0.25 }
}

fn cue(kind: CueKind) -> Cue {
    Cue::plain(kind, 1.0).unwrap()
}

#[test]
fn resolve_returns_the_bound_action_for_guessed_chosen_and_degraded() {
    let mut b = Bindings::default();
    b.set(CueKind::Offer, Binding::Guessed { action: motion("offer"), pattern: "offer".into() });
    b.set(CueKind::Open, Binding::Chosen { action: motion("open"), evidence: Evidence::UserPreviewed });
    b.set(
        CueKind::BeatBoth,
        Binding::Degraded { action: motion("beat"), why: Degradation::PlaysAs(CueKind::Beat) },
    );
    assert_eq!(resolve(&cue(CueKind::Offer), &b), Ok(motion("offer")));
    assert_eq!(resolve(&cue(CueKind::Open), &b), Ok(motion("open")));
    assert_eq!(resolve(&cue(CueKind::BeatBoth), &b), Ok(motion("beat")));
}

#[test]
fn resolve_unbound_is_an_error_and_not_nothing() {
    let b = Bindings::default();
    assert_eq!(resolve(&cue(CueKind::Offer), &b), Err(Unresolved::Unbound));
}

#[test]
fn resolve_declared_absent_is_nothing_and_not_an_error() {
    let mut b = Bindings::default();
    b.set(CueKind::Offer, Binding::DeclaredAbsent);
    assert_eq!(resolve(&cue(CueKind::Offer), &b), Ok(CharacterAction::Nothing));
}

#[test]
fn set_replaces_only_that_slot() {
    let mut b = Bindings::default();
    b.set(CueKind::Open, Binding::DeclaredAbsent);
    for (kind, binding) in b.iter() {
        let want = if kind == CueKind::Open { Binding::DeclaredAbsent } else { Binding::Unbound };
        assert_eq!(binding, &want, "slot {kind:?}");
    }
}

#[test]
fn bindings_refuse_a_key_that_is_not_a_cue_kind() {
    let msg = serde_json::from_value::<Bindings>(json!({ "wave": { "state": "unbound" } })).unwrap_err().to_string();
    assert!(msg.contains("unknown variant `wave`"), "{msg}");
}

#[test]
fn bindings_refuse_the_same_key_twice() {
    let text = r#"{ "offer": { "state": "unbound" }, "offer": { "state": "declaredAbsent" } }"#;
    let msg = serde_json::from_str::<Bindings>(text).unwrap_err().to_string();
    assert!(msg.contains("duplicate binding for cue kind Offer"), "{msg}");
}

#[test]
fn the_page_reads_camel_case_fields_on_an_action() {
    let wire = serde_json::to_value(motion("wave")).unwrap();
    assert_eq!(wire, json!({ "do": "playMotion", "group": "wave", "index": 0, "fadeIn": 0.25 }));
    let nothing: Value = serde_json::to_value(CharacterAction::Nothing).unwrap();
    assert_eq!(nothing, json!({ "do": "nothing" }));
}
