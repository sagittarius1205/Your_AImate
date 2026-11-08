// The wire format. One JSON object per line; stdin and stdout carry only these, so nothing else
// may ever be written to stdout.

use crate::character::{Character, Greeting};
use crate::voice::Voice;
use serde::{Deserialize, Serialize};

/// What a line can ask for. Read through `Wire`, so a `Character` or a `Greeting` that is in a
/// `Request` has already passed its limits, and a line that breaks them is a malformed line like
/// any other.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(try_from = "Wire")]
pub enum Request {
    Say { text: String },
    /// Takes effect with the next turn: a turn already queued ahead of it is answered as the
    /// character it was asked of.
    Character(Character),
    /// Voiced in the current voice without asking the model.
    Greet(Greeting),
    Shutdown,
}

/// The line as it is written, before its fields are checked.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Wire {
    Say { text: String },
    Character { name: String, persona: String, voice: Option<String> },
    Greet { text: String },
    Shutdown,
}

impl TryFrom<Wire> for Request {
    type Error = String;

    fn try_from(wire: Wire) -> Result<Request, String> {
        Ok(match wire {
            Wire::Say { text } => Request::Say { text },
            Wire::Character { name, persona, voice } => Request::Character(Character::new(&name, &persona, voice.as_deref())?),
            Wire::Greet { text } => Request::Greet(Greeting::new(&text)?),
            Wire::Shutdown => Request::Shutdown,
        })
    }
}

#[derive(Serialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    Ready { version: &'static str },
    /// One piece of a turn's answer. `part` counts from 0 within the turn, and `turn_end` follows
    /// the last.
    Utterance { part: usize, text: String, tags: Vec<String>, voice: Voice },
    TurnEnd,
    Error { message: String },
}

/// Bytes, not `&str`, so a line that is not UTF-8 is a malformed line like any other and not a
/// reason for the read loop to fail.
pub fn parse(line: &[u8]) -> Result<Request, String> {
    let line = std::str::from_utf8(line).map_err(|e| format!("input is not valid UTF-8: {e}"))?;
    serde_json::from_str(line).map_err(|e| format!("bad request: {e}"))
}

/// What a turn, or one piece of it, says before it has a voice. `text` carries each gesture's tag
/// in front of its clause, the form the Unreal side parses; `tags` is every gesture in `text`, once.
#[derive(Debug, PartialEq)]
pub struct Said {
    pub text: String,
    pub tags: Vec<String>,
}

impl Said {
    /// The words to synthesise: `text` without its `[tag]` markers, by the same rule the page's
    /// `spoken()` uses for the bubble (`/\[[A-Za-z]+\] ?/g`, then trimmed), so what is heard is
    /// what is read.
    pub fn spoken(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        let mut rest = self.text.as_str();
        while let Some(open) = rest.find('[') {
            let after = &rest[open + 1..];
            let letters = after.bytes().take_while(u8::is_ascii_alphabetic).count();
            if letters > 0 && after.as_bytes().get(letters) == Some(&b']') {
                out.push_str(&rest[..open]);
                let next = &after[letters + 1..];
                rest = next.strip_prefix(' ').unwrap_or(next);
            } else {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
        out.push_str(rest);
        out.trim().to_string()
    }

    /// This piece on the wire, as part `part` of its turn.
    pub fn utterance(self, part: usize, voice: Voice) -> Event {
        Event::Utterance { part, text: self.text, tags: self.tags, voice }
    }
}
