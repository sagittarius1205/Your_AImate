// What the model may say, and the checks between its answer and the wire.
//
// The model is asked for `{clauses: [{text, gesture}], language, affect}`. `--json-schema` asks for
// that shape, and nothing in here relies on the other side having kept its word: the promises this
// protocol makes (no square bracket in the spoken text, gestures from a closed list) have to hold
// even when the model, or a future version of the CLI, does not.

use crate::character::Character;
use crate::protocol::Said;
use crate::relay::Fact;
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

/// The most clauses an answer may have. The schema asks for it and `parse` holds it, so the limit
/// does not depend on whether the endpoint enforces `maxItems`.
pub const MAX_CLAUSES: usize = 3;

/// Whether this core offers the character a way to ask for a relay. Fixed for the life of the
/// process the character runs in, because `claude` is started with the schema and the prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relaying {
    Off,
    On,
}

pub const RELAY_TO_MAX: usize = 64;
pub const RELAY_TEXT_MAX: usize = 1000;

/// The shape handed to `--json-schema`. Built from `Gesture::ALL`, so the vocabulary the model is
/// shown and the one the core accepts are one list. `Off` is the schema from before relaying, byte
/// for byte, so the VR client sees no change. `On` adds `relay`, listed in `required` and nullable,
/// because strict mode on the HTTP engine wants every property required.
pub fn schema(relaying: Relaying) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "clauses": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_CLAUSES,
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
    });
    if relaying == Relaying::On {
        schema["properties"]["relay"] = json!({
            "anyOf": [
                { "type": "null" },
                {
                    "type": "object",
                    "properties": { "to": { "type": "string" }, "text": { "type": "string" } },
                    "required": ["to", "text"],
                    "additionalProperties": false,
                },
            ],
        });
        schema["required"] = json!(["clauses", "language", "affect", "relay"]);
    }
    schema
}

/// The system prompt appended to the CLI's own: a sentence naming her, the profile's persona, and
/// then the rules. The persona is the profile's to write and the rules are not, so it comes first
/// and the rules have the last word. Nothing here takes a session, so no session's title can reach
/// the prompt.
pub fn prompt(character: &Character, relaying: Relaying) -> String {
    let persona = character.persona().trim();
    // A persona that stops without a full stop would run into the first rule as one sentence.
    let stop = if persona.ends_with(['。', '！', '？', '…', '.', '!', '?']) { "" } else { "。" };
    let relay = if relaying == Relaying::On { RELAY_RULES } else { "" };
    format!("你是一個桌面寵物角色，名字叫{}。{persona}{stop}{}{relay}", character.name(), rules())
}

/// What she is told about relaying when it is offered. She never sees a session: she writes the
/// user's words for the target and the text, and the pet does the rest. Nothing she writes on a
/// relay turn is said, so she cannot claim the delivery there, and she is told not to claim it
/// anywhere else.
const RELAY_RULES: &str = "你自己沒辦法傳話給其他 Claude session，也看不到有哪些 session。\
使用者要你轉話給某個 session 時，relay 填 {to, text}：to 是他對那個 session 的叫法，照他說的寫，他沒說就留空；\
text 是要轉過去的話，照他要轉的內容，用他的口吻和他說的語言寫，不要加他沒說的話。這一輪的 clauses 不會念出來，寫一句就好，桌寵會接手。\
沒有要轉話的時候 relay 填 null。不要說你已經傳了、送了或轉達了，送沒送到只有桌寵知道，它會告訴使用者。\
要送出一定要使用者在畫面上按「送出」，用說的不算。";

/// The sentence put in front of the next thing the user says, so she knows how the last relay
/// ended. Fixed words, with no title and no text from another session.
pub fn note(fact: Fact) -> &'static str {
    match fact {
        Fact::Delivered => "（桌寵告訴你的，不是使用者說的：你上一次要轉的話已經放進對方 session 的收件匣，對方還沒讀，也可能不照做。）",
        Fact::NotSent => "（桌寵告訴你的，不是使用者說的：你上一次要轉的話沒有送出。）",
        Fact::Unknown => "（桌寵告訴你的，不是使用者說的：你上一次要轉的話送沒送到不確定。）",
    }
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

impl Clause {
    /// This clause cut into atoms: the cut `Reply::atoms` makes of it inside a whole answer.
    pub fn atoms(&self) -> Vec<Atom> {
        clause_atoms(self)
    }
}

/// An answer that passed every check. Holding one is the proof: the only way to get a `Reply` is
/// `parse`.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub clauses: Vec<Clause>,
    /// `Some` only when relaying is offered and she filled it.
    pub relay: Option<RelayAsk>,
    /// As the model wrote them, for the dialogue record. Nothing is said or checked by them.
    pub language: Option<String>,
    pub affect: Option<String>,
}

/// What she asked to have passed on, in the user's words. `to` may be empty, and it is never a
/// title, because she never sees one. Made only by `parse`.
#[derive(Debug, PartialEq)]
pub struct RelayAsk {
    to: String,
    text: String,
}

impl RelayAsk {
    pub fn to(&self) -> &str {
        &self.to
    }

    pub fn text(&self) -> &str {
        &self.text
    }
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
/// says which clause and which rule. `Off` ignores a `relay` key. `On` reads it: null or absent is
/// no relay, because an engine that ignores the schema leaves it out, and anything else has to be a
/// relay that keeps its limits or the turn is refused. `language` and `affect` are taken when they
/// are strings and never refuse a turn.
pub fn parse(result: &str, relaying: Relaying) -> Result<Reply, String> {
    let value: Value = serde_json::from_str(result)
        .map_err(|e| format!("the answer is not JSON ({e}): {}", excerpt(result)))?;
    let Value::Object(object) = &value else {
        return Err(format!(
            "the answer is JSON but not an object, it is {}: {}",
            kind(&value),
            excerpt(result)
        ));
    };
    let Some(Value::Array(clauses)) = object.get("clauses") else {
        return Err(format!("the answer has no `clauses` array: {}", excerpt(result)));
    };
    if clauses.is_empty() {
        return Err("the answer has an empty `clauses` array, so there is nothing to say".to_string());
    }
    if clauses.len() > MAX_CLAUSES {
        return Err(format!("the answer has {} clauses, and an answer has at most {MAX_CLAUSES}", clauses.len()));
    }
    let clauses = clauses.iter().enumerate().map(|(at, clause)| parse_clause(at, clause)).collect::<Result<Vec<_>, _>>()?;
    let relay = match (relaying, object.get("relay")) {
        (Relaying::Off, _) | (Relaying::On, None | Some(Value::Null)) => None,
        (Relaying::On, Some(value)) => Some(parse_relay(value)?),
    };
    let text = |name: &str| object.get(name).and_then(Value::as_str).map(str::to_string);
    Ok(Reply { clauses, relay, language: text("language"), affect: text("affect") })
}

fn parse_relay(value: &Value) -> Result<RelayAsk, String> {
    let Value::Object(object) = value else {
        return Err(format!("relay is {}, not an object or null", kind(value)));
    };
    let field = |name: &str| match object.get(name) {
        Some(Value::String(text)) => Ok(text.trim().to_string()),
        _ => Err(format!("relay.{name} is missing or not a string")),
    };
    let (to, text) = (field("to")?, field("text")?);
    if to.chars().count() > RELAY_TO_MAX {
        return Err(format!("relay.to is {} characters, and the limit is {RELAY_TO_MAX}", to.chars().count()));
    }
    if text.is_empty() {
        return Err("relay.text is empty, so there is nothing to pass on".to_string());
    }
    if text.chars().count() > RELAY_TEXT_MAX {
        return Err(format!("relay.text is {} characters, and the limit is {RELAY_TEXT_MAX}", text.chars().count()));
    }
    for (name, value, lines) in [("to", &to, false), ("text", &text, true)] {
        if let Some(c) = value.chars().find(|&c| c.is_control() && !(lines && matches!(c, '\n' | '\t'))) {
            return Err(format!("relay.{name} contains the control character {c:?}"));
        }
    }
    Ok(RelayAsk { to, text })
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

/// The first clause of an answer that is still being written, read from the pieces of JSON the
/// model streams (`claude.rs`). It is handed over the moment its object closes, and only once
/// `parse_clause` has taken it as clause 0, so nothing `parse` would refuse in that clause gets out
/// of here. It proves nothing about the rest of the answer: the later clauses, their count and the
/// relay are `parse`'s to judge once the answer is whole.
///
/// A piece can end anywhere, inside a key, an escape or a character, so the text is kept and the
/// scan goes on from where it stopped. Only the answer's own top-level `clauses` counts, not one
/// nested in another value or written inside a string. JSON that breaks off is never repaired: a
/// clause that has not closed is not there.
pub struct FirstClause {
    text: String,
    scanned: usize,
    /// Containers open around the byte being scanned. The answer's object is 1.
    depth: usize,
    /// Where the string being scanned opened, while the scan is inside one.
    string: Option<usize>,
    escaped: bool,
    seek: Seek,
}

/// What the scan is looking for, outside strings.
#[derive(Clone, Copy)]
enum Seek {
    /// The answer's opening brace.
    Answer,
    /// A top-level key, or the comma before one.
    Key,
    /// The colon after a top-level key, and whether that key is `clauses`.
    Colon { clauses: bool },
    /// The end of a top-level value that is not `clauses`.
    Skip,
    /// The `[` of `clauses`.
    Array,
    /// The first element of `clauses`.
    Element,
    /// The end of the first element, which opened at this offset.
    Clause(usize),
}

/// What one more piece made of the first clause.
pub enum Streamed {
    /// Not closed yet. The next piece goes into this.
    Open(FirstClause),
    /// Closed, and taken as clause 0.
    Closed(Clause),
    /// It will not come out of this stream, and why. The whole answer is still `parse`'s to judge.
    Never(String),
}

impl FirstClause {
    pub fn new() -> FirstClause {
        FirstClause { text: String::new(), scanned: 0, depth: 0, string: None, escaped: false, seek: Seek::Answer }
    }

    pub fn push(mut self, piece: &str) -> Streamed {
        self.text.push_str(piece);
        while self.scanned < self.text.len() {
            let at = self.scanned;
            self.scanned += 1;
            match self.step(at) {
                Ok(None) => {}
                Ok(Some(clause)) => return Streamed::Closed(clause),
                Err(why) => return Streamed::Never(why),
            }
        }
        Streamed::Open(self)
    }

    /// One byte. Every byte that means something to the structure is ASCII, and no byte of a
    /// multi-byte character is, so a byte scan never mistakes part of a character for structure and
    /// every offset kept is a character boundary.
    fn step(&mut self, at: usize) -> Result<Option<Clause>, String> {
        let byte = self.text.as_bytes()[at];
        if let Some(open) = self.string {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.string = None;
                if let Seek::Key = self.seek {
                    // Decoded, so an escaped key is the key it spells, as it is to `parse`.
                    let name: String = serde_json::from_str(&self.text[open..=at]).map_err(|e| format!("a key of the answer is not JSON ({e})"))?;
                    self.seek = Seek::Colon { clauses: name == "clauses" };
                }
            }
            return Ok(None);
        }
        if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            return Ok(None);
        }
        match (self.seek, byte) {
            (Seek::Answer, b'{') => {
                self.depth = 1;
                self.seek = Seek::Key;
            }
            (Seek::Answer, _) => return Err("the answer is not an object".to_string()),
            (Seek::Key | Seek::Skip | Seek::Clause(_), b'"') => self.string = Some(at),
            (Seek::Key, b',') => {}
            (Seek::Key, b'}') => return Err("the answer has no `clauses`".to_string()),
            (Seek::Colon { clauses }, b':') => self.seek = if clauses { Seek::Array } else { Seek::Skip },
            (Seek::Array, b'[') => {
                self.depth = 2;
                self.seek = Seek::Element;
            }
            (Seek::Array, _) => return Err("`clauses` is not an array".to_string()),
            (Seek::Element, b'{') => {
                self.depth = 3;
                self.seek = Seek::Clause(at);
            }
            (Seek::Element, b']') => return Err("the `clauses` array is empty".to_string()),
            (Seek::Element, _) => return Err("clauses[0] is not an object".to_string()),
            (Seek::Skip | Seek::Clause(_), b'{' | b'[') => self.depth += 1,
            (Seek::Skip, b',') if self.depth == 1 => self.seek = Seek::Key,
            (Seek::Skip, b'}') if self.depth == 1 => return Err("the answer has no `clauses`".to_string()),
            (Seek::Skip | Seek::Clause(_), b'}' | b']') if self.depth > 1 => {
                self.depth -= 1;
                if let (Seek::Clause(open), 2) = (self.seek, self.depth) {
                    let value: Value = serde_json::from_str(&self.text[open..=at]).map_err(|e| format!("clauses[0] is not JSON ({e})"))?;
                    return parse_clause(0, &value).map(Some);
                }
            }
            (Seek::Skip | Seek::Clause(_), b'}' | b']') => return Err("the answer's JSON is broken".to_string()),
            (Seek::Skip | Seek::Clause(_), _) => {}
            (Seek::Key | Seek::Colon { .. }, _) => return Err(format!("the answer's JSON is broken at {:?}", char::from(byte))),
        }
        Ok(None)
    }
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
