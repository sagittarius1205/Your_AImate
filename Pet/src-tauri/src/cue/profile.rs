//! What each cue does on the figure v1 ships (Docs/DESIGN_Pet.md 3.1), as an authored profile
//! checked in beside this file (`figure.json`).
//!
//! Every lane is a parameter the figure's `rig.json` declares and every number is in that
//! parameter's own unit. The numbers began as the poses rendered on Mao in the gesture investigation
//! (Docs/DESIGN_Pet.md 4.1.1), and the rig's parameters were sized so they read the same on the
//! puppet (3.4). A nod's lanes are per degree of nod and a glance's per degree of yaw or pitch, so
//! the cue's own numbers size them (`vrcue::resolve`).
//!
//! The profile carries a fingerprint of the parameters it was written for: each parameter's id,
//! range and default, in the rig's order. Not of the whole rig.json, because the numbers are in the
//! parameters' units and a rig that only moves its nodes differently for them, such as the gown
//! that holds a book, is a body they were written for. A rig that declares other parameters gets
//! every slot `Unbound` instead, so its cues come back `Unresolved`, a count the page can see, and
//! nothing moves on a body the numbers were not written for.

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;
use vrcue::Bindings;

/// The figure this profile was written for, the bundle's default (`characters/index.json`).
/// `pet_gesture` checks the two agree.
pub const MODEL: &str = "characters/char-20261107/rig.json";

const RAW: &str = include_str!("figure.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub model: String,
    /// `parameters(rig)` of the rig the numbers were written for.
    pub parameters: String,
    pub bindings: Bindings,
}

/// The checked-in profile, parsed once. A malformed one is a bug in this commit, not a run-time
/// condition, so it panics with the reason, as `voice::bank` does.
pub fn figure() -> &'static Profile {
    static PROFILE: OnceLock<Profile> = OnceLock::new();
    PROFILE.get_or_init(|| {
        let profile: Profile =
            serde_json::from_str(RAW).unwrap_or_else(|e| panic!("cue/figure.json is not a profile: {e}"));
        assert_eq!(profile.model, MODEL, "cue/figure.json is written for another figure");
        profile
    })
}

/// FNV-1a, 64 bits, over the file's bytes. Not a security check: it tells one rig.json from
/// another, and it needs no dependency to do it.
pub fn fingerprint(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3));
    format!("fnv1a64:{hash:016x}")
}

/// The fingerprint of what a rig declares: one line per parameter, its id, min, max and default as
/// JSON writes them, in the rig's order. The order is part of it because the page indexes the
/// parameters by position.
pub fn parameters(rig: &[u8]) -> Result<String, String> {
    let rig: Value = serde_json::from_slice(rig).map_err(|e| format!("the rig is not JSON: {e}"))?;
    let list = rig.get("parameters").and_then(Value::as_array).ok_or("the rig declares no parameters")?;
    let mut text = String::new();
    for p in list {
        for key in ["id", "min", "max", "default"] {
            text.push_str(&p.get(key).map(Value::to_string).unwrap_or_default());
            text.push('\t');
        }
        text.push('\n');
    }
    Ok(fingerprint(text.as_bytes()))
}

/// The bindings for the figure whose rig.json is `rig`: the profile's when it declares the
/// parameters the profile was written for, and the reason it does not otherwise.
pub fn bindings_for(rig: &[u8]) -> Result<&'static Bindings, String> {
    let profile = figure();
    let found = parameters(rig)?;
    if found == profile.parameters {
        Ok(&profile.bindings)
    } else {
        Err(format!("its parameters are {found}, and the profile is for {}", profile.parameters))
    }
}
