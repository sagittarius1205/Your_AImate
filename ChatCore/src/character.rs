// Who the character is, as far as the core is concerned: a name, the text that says how she is and
// talks, and the voice she is spoken in. The pet reads a profile (Docs/DESIGN_Characters.md) and
// sends these three over the wire; the fixed rules about hearing, speaking, clauses and gestures
// stay in `reply.rs`, where a profile cannot reach them.
//
// A `Character` can only be made through `Character::new`, so holding one is the proof that its
// limits held. The limits are the profile's (section 2 of that document), checked again here
// because the line comes from a process the core does not control.

pub const NAME_MAX: usize = 32;
pub const PERSONA_MAX: usize = 2000;
pub const VOICE_MAX: usize = 48;
pub const GREETING_MAX: usize = 200;

/// The voice every character without one of its own is spoken in, and the one the speech service
/// ships with.
pub const DEFAULT_VOICE: &str = "ref_voice_lively_f";

/// What 小克 is, in her own words. It is the sentences the persona used to open with, so the
/// default character's prompt is the one this core has always sent.
const DEFAULT_PERSONA: &str =
    "你住在使用者的桌面角落，講話短、口語，預設用繁體中文，對方用別的語言就用那個語言。";

/// A voice id as the speech service reads it: `^[a-z0-9][a-z0-9_-]{0,47}$`. Either the default
/// voice or a character's id, which is a folder name (Docs/DESIGN_Characters.md section 1), so the
/// same rule keeps a path out of the service's lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceId(String);

impl VoiceId {
    pub fn parse(id: &str) -> Result<VoiceId, String> {
        let mut bytes = id.bytes();
        let first_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        let rest_ok = bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !first_ok || !rest_ok || id.len() > VOICE_MAX {
            return Err(format!(
                "voice {} is not a voice id: it is 1 to {VOICE_MAX} characters of a-z, 0-9, _ and -, and does not start with _ or -",
                crate::reply::excerpt(id)
            ));
        }
        Ok(VoiceId(id.to_string()))
    }

    pub fn standard() -> VoiceId {
        VoiceId(DEFAULT_VOICE.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Character {
    name: String,
    persona: String,
    voice: VoiceId,
}

impl Character {
    /// 小克, who the core is until the pet says otherwise.
    pub fn standard() -> Character {
        Character { name: "小克".to_string(), persona: DEFAULT_PERSONA.to_string(), voice: VoiceId::standard() }
    }

    /// A `voice` of `None` is the default voice, not "the one before": a profile with no voice of
    /// its own must not keep speaking in the previous character's.
    pub fn new(name: &str, persona: &str, voice: Option<&str>) -> Result<Character, String> {
        within("name", name, NAME_MAX)?;
        // The name is put into a sentence of its own, and a line break would end that sentence.
        if let Some(bad) = name.chars().find(|c| c.is_control()) {
            return Err(format!("name contains the control character {bad:?}, and it is one line"));
        }
        within("persona", persona, PERSONA_MAX)?;
        let voice = voice.map_or_else(|| Ok(VoiceId::standard()), VoiceId::parse)?;
        Ok(Character { name: name.to_string(), persona: persona.to_string(), voice })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn persona(&self) -> &str {
        &self.persona
    }

    pub fn voice(&self) -> &VoiceId {
        &self.voice
    }
}

/// Something she says without asking the model: the greeting a profile carries. Counted in
/// characters, and kept free of square brackets for the reason `reply::parse` gives for the model's
/// clauses: the planner's tag stripper swallows what is between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Greeting(String);

impl Greeting {
    pub fn new(text: &str) -> Result<Greeting, String> {
        within("text", text, GREETING_MAX)?;
        if let Some(bracket) = text.chars().find(|c| matches!(c, '[' | ']')) {
            return Err(format!("text contains {bracket:?}, which would swallow part of what is said"));
        }
        Ok(Greeting(text.to_string()))
    }

    pub fn into_text(self) -> String {
        self.0
    }
}

/// `value` must say something and be at most `max` characters, counted as characters and not bytes
/// because a Chinese name is three bytes a character.
fn within(field: &str, value: &str, max: usize) -> Result<(), String> {
    let chars = value.chars().count();
    if value.trim().is_empty() {
        return Err(format!("{field} is empty"));
    }
    if chars > max {
        return Err(format!("{field} is {chars} characters, and the limit is {max}"));
    }
    Ok(())
}
