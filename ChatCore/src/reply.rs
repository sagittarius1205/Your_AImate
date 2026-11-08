// What the model may say, and the checks between its answer and the wire.
//
// The model is asked for `{clauses: [{text, gesture}], language, affect}`. `--json-schema` asks for
// that shape, and nothing in here relies on the other side having kept its word: the promises this
// protocol makes (no square bracket in the spoken text, gestures from a closed list) have to hold
// even when the model, or a future version of the CLI, does not.

use crate::character::Character;
use crate::protocol::Said;
use serde_json::{Value, json};

/// The gestures a model may pick. Not every cue the pet can play: a beat, a two-handed beat and the
/// rest pose come from the audio and the gaps between gestures, so letting the model choose them
/// would let it decide something it cannot see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    None,
    SelfRef,
    Offer,
    Open,
    Question,
    Nod,
    LookAway,
}

impl Gesture {
    pub const ALL: [Gesture; 7] = [
        Gesture::None,
        Gesture::SelfRef,
        Gesture::Offer,
        Gesture::Open,
        Gesture::Question,
        Gesture::Nod,
        Gesture::LookAway,
    ];

    /// What the model writes in `gesture`. These are the names the pet's `CueKind` serialises to
    /// (camelCase), because that is where a gesture goes next.
    pub const fn wire(self) -> &'static str {
        match self {
            Gesture::None => "none",
            Gesture::SelfRef => "selfRef",
            Gesture::Offer => "offer",
            Gesture::Open => "open",
            Gesture::Question => "question",
            Gesture::Nod => "nod",
            Gesture::LookAway => "lookAway",
        }
    }

    /// The tag put in front of the clause for the Unreal side, or `None` for no tag.
    ///
    /// Each spelling is one that `VRGesturePlanner` accepts: `GestureForTag` for the four hand
    /// gestures, and `nod` and `think` where the planner reads them itself. The authority is
    /// `CueKind::tags()` in `Pet/crates/vrcue/src/lib.rs`, which this crate does not depend on (it
    /// lives under `Pet/`, and this core also serves the VR side). The table is repeated here and
    /// `gesture_tags_are_the_ones_the_planner_accepts` pins it. A tag the planner does not know is
    /// not an error there, it is dropped from the speech without a word, so a wrong spelling in this
    /// table would make a gesture vanish silently.
    pub const fn tag(self) -> Option<&'static str> {
        match self {
            Gesture::None => None,
            Gesture::SelfRef => Some("self"),
            Gesture::Offer => Some("offer"),
            Gesture::Open => Some("open"),
            Gesture::Question => Some("question"),
            Gesture::Nod => Some("nod"),
            Gesture::LookAway => Some("think"),
        }
    }

    /// One line for the persona: when the model should pick this gesture. A match with no wildcard,
    /// so a gesture added above cannot reach the model without a sentence saying what it is for.
    const fn meaning(self) -> &'static str {
        match self {
            Gesture::None => "不做手勢",
            Gesture::SelfRef => "指自己，說到「我」的時候",
            Gesture::Offer => "伸手向對方，說到「你」的時候",
            Gesture::Open => "雙手攤開，說到大家、這裡、一起的時候",
            Gesture::Question => "聳肩或疑問的時候",
            Gesture::Nod => "點頭，同意或打招呼的時候",
            Gesture::LookAway => "移開視線想一下的時候",
        }
    }

    pub fn from_wire(name: &str) -> Option<Gesture> {
        Gesture::ALL.into_iter().find(|g| g.wire() == name)
    }

    fn vocabulary() -> String {
        Gesture::ALL.map(Gesture::wire).join(", ")
    }
}

/// The shape handed to `--json-schema`. Built from `Gesture::ALL`, so the vocabulary the model is
/// shown and the one the core accepts are one list.
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "clauses": {
                "type": "array",
                "minItems": 1,
                "maxItems": 3,
                "items": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" },
                        "gesture": { "type": "string", "enum": Gesture::ALL.map(Gesture::wire) },
                    },
                    "required": ["text", "gesture"],
                    "additionalProperties": false,
                },
            },
            "language": { "type": "string" },
            "affect": { "type": "string" },
        },
        "required": ["clauses", "language", "affect"],
        "additionalProperties": false,
    })
}

/// The system prompt appended to the CLI's own: a sentence naming her, the profile's persona, and
/// then the rules. The persona is the profile's to write and the rules are not, so it comes first
/// and the rules have the last word.
pub fn prompt(character: &Character) -> String {
    let persona = character.persona().trim();
    // A persona that stops without a full stop would run into the first rule as one sentence.
    let stop = if persona.ends_with(['。', '！', '？', '…', '.', '!', '?']) { "" } else { "。" };
    format!("你是一個桌面寵物角色，名字叫{}。{persona}{stop}{}", character.name(), rules())
}

/// What holds for every character: that she hears and speaks, how an answer is cut into clauses,
/// and the gestures and the bracket rule the checks below enforce. The gesture list comes from the
/// same table as the schema, so the prompt cannot name a gesture the core would then reject.
fn rules() -> String {
    let gestures: Vec<String> = Gesture::ALL.iter().map(|g| format!("{}（{}）", g.wire(), g.meaning())).collect();
    format!(
        "你聽得到也會說話：使用者常常是對著麥克風跟你講話，你收到的是他的話\
         轉成的文字，你的回答會用你自己的聲音念出來。所以不要說你聽不到、看不到聲音或只能打字。\
         轉出來的字可能有錯字或是簡體字，照意思理解，不用糾正他。每次回答拆成一到三個子句，每個子句挑一個手勢。\
         手勢只能是這幾個：{}。方括號不可以出現在 text 裡。",
        gestures.join("、"),
    )
}

#[derive(Debug, PartialEq)]
pub struct Clause {
    pub text: String,
    pub gesture: Gesture,
}

/// An answer that passed every check. Holding one is the proof: the only way to get a `Reply` is
/// `parse`.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub clauses: Vec<Clause>,
}

impl Reply {
    /// What the whole turn says. The same join as any run of its atoms, so a turn sent in one piece
    /// and a turn sent in several say the same words.
    pub fn said(&self) -> Said {
        said(&self.atoms())
    }

    /// The pieces the turn may be cut into, in order: every clause boundary is a cut, and a clause
    /// is cut again after a run of punctuation. See `clause_atoms` for which runs.
    pub fn atoms(&self) -> Vec<Atom> {
        self.clauses.iter().flat_map(clause_atoms).collect()
    }
}

/// The smallest piece an answer is ever cut into. A run of atoms is what one `utterance` says.
#[derive(Debug, PartialEq)]
pub struct Atom {
    start: Start,
    text: String,
    /// What the pace counts in: every character that is neither punctuation nor whitespace is one,
    /// which for Chinese is one per character. The fit the starting pace came from counted this way.
    pub syllables: usize,
}

#[derive(Debug, PartialEq)]
enum Start {
    /// The first atom of a clause, which carries the clause's gesture.
    Clause(Gesture),
    /// A later atom of the same clause, after the whitespace that separated it from the one before
    /// (usually none). A run that starts here drops that whitespace.
    Inside { gap: String },
}

/// What a run of atoms says: each clause the run opens with its tag in front, clauses joined by a
/// space (a clause does not always end in punctuation, and English needs the space), and a clause
/// the run enters part way through without its tag. `tags` is every tag in the text, once each, in
/// the order they first appear.
pub fn said(atoms: &[Atom]) -> Said {
    let mut text = String::new();
    let mut tags: Vec<String> = Vec::new();
    for (at, atom) in atoms.iter().enumerate() {
        match &atom.start {
            Start::Clause(gesture) => {
                if at > 0 {
                    text.push(' ');
                }
                if let Some(tag) = gesture.tag() {
                    if !tags.iter().any(|t| t == tag) {
                        tags.push(tag.to_string());
                    }
                    text.push_str(&format!("[{tag}]"));
                }
            }
            Start::Inside { gap } if at > 0 => text.push_str(gap),
            Start::Inside { .. } => {}
        }
        text.push_str(&atom.text);
    }
    Said { text, tags }
}

/// What the echo engine says: the sentence exactly as it came, as one atom, so an echo is never cut.
pub fn echo(text: String) -> Vec<Atom> {
    vec![Atom { start: Start::Clause(Gesture::None), syllables: syllables(&text), text }]
}

/// The marks `Pet/src-tauri/src/cue/align.rs::is_punctuation` splits speech spans at, which is
/// `IsPunctuation` in VRGesturePlan.cpp.
fn is_punctuation(c: char) -> bool {
    "，。？！、；：,.?!;:…".contains(c)
}

fn syllables(text: &str) -> usize {
    text.chars().filter(|&c| !is_punctuation(c) && !c.is_whitespace()).count()
}

/// A clause cut after every run of punctuation marks, except where a cut would leave a side with no
/// syllable, and except after an ASCII mark that is not followed by whitespace, so "3.5" and "e.g"
/// stay whole. The whitespace after a cut belongs to neither side.
fn clause_atoms(clause: &Clause) -> Vec<Atom> {
    let text = clause.text.as_str();
    let mut atoms = Vec::new();
    let mut start = Start::Clause(clause.gesture);
    let mut from = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((_, mark)) = chars.next() {
        let Some(&(end, after)) = chars.peek() else { break };
        let cut = is_punctuation(mark) && !is_punctuation(after) && (!mark.is_ascii() || after.is_whitespace());
        let rest = text[end..].trim_start();
        if !cut || syllables(&text[from..end]) == 0 || syllables(rest) == 0 {
            continue;
        }
        let atom = &text[from..end];
        atoms.push(Atom { start, text: atom.to_string(), syllables: syllables(atom) });
        let resume = text.len() - rest.len();
        start = Start::Inside { gap: text[end..resume].to_string() };
        from = resume;
    }
    atoms.push(Atom { start, text: text[from..].to_string(), syllables: syllables(&text[from..]) });
    atoms
}

/// The second decode. `claude` hands over its answer as a string that holds the JSON, so this takes
/// that string and not a value. Rejects the whole turn at the first clause that breaks a rule, and
/// says which clause and which rule.
pub fn parse(result: &str) -> Result<Reply, String> {
    let value: Value = serde_json::from_str(result)
        .map_err(|e| format!("claude's result is not JSON ({e}): {}", excerpt(result)))?;
    let Value::Object(object) = &value else {
        return Err(format!(
            "claude's result is JSON but not an object, it is {}: {}",
            kind(&value),
            excerpt(result)
        ));
    };
    let Some(Value::Array(clauses)) = object.get("clauses") else {
        return Err(format!("claude's result has no `clauses` array: {}", excerpt(result)));
    };
    if clauses.is_empty() {
        return Err("claude's result has an empty `clauses` array, so there is nothing to say".to_string());
    }
    clauses
        .iter()
        .enumerate()
        .map(|(at, clause)| parse_clause(at, clause))
        .collect::<Result<Vec<_>, _>>()
        .map(|clauses| Reply { clauses })
}

fn parse_clause(at: usize, value: &Value) -> Result<Clause, String> {
    let Value::Object(object) = value else {
        return Err(format!("clauses[{at}] is {}, not an object", kind(value)));
    };
    let Some(Value::String(text)) = object.get("text") else {
        return Err(format!("clauses[{at}].text is missing or not a string"));
    };
    let Some(Value::String(name)) = object.get("gesture") else {
        return Err(format!("clauses[{at}].gesture is missing or not a string"));
    };
    let Some(gesture) = Gesture::from_wire(name) else {
        return Err(format!(
            "clauses[{at}].gesture {} is outside the vocabulary ({})",
            excerpt(name),
            Gesture::vocabulary()
        ));
    };
    // The planner's tag stripper swallows anything between a `[` and the next `]`, so a bracket
    // that got through would make part of a sentence disappear without an error.
    if let Some(bracket) = text.chars().find(|c| matches!(c, '[' | ']')) {
        return Err(format!("clauses[{at}].text contains {bracket:?}: {}", excerpt(text)));
    }
    if text.trim().is_empty() {
        return Err(format!("clauses[{at}].text is empty, so the clause says nothing"));
    }
    Ok(Clause { text: text.trim().to_string(), gesture })
}

pub fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

const EXCERPT_CHARS: usize = 80;

/// The start of something the other side sent, for an error message: enough to recognise it, short
/// enough to read in a bubble.
pub fn excerpt(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(EXCERPT_CHARS).collect();
    if chars.next().is_some() { format!("{head:?}...") } else { format!("{head:?}") }
}
