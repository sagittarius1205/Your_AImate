// The wire format. One JSON object per line; stdin and stdout carry only these, so nothing else
// may ever be written to stdout.

use crate::character::{Character, Greeting};
use crate::dialogue::Keep;
use crate::relay::{self, Nonce, Order};
use crate::voice::Voice;
use serde::{Deserialize, Serialize};

/// What a line can ask for. Read through `Wire`, so a `Character` or a `Greeting` that is in a
/// `Request` has already passed its limits, and a line that breaks them is a malformed line like
/// any other.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(try_from = "Wire")]
pub enum Request {
    /// `keep` is the line's `record`, there only when the client wants this turn in the dialogue
    /// record (`dialogue.rs`). Without it nothing of the turn is kept.
    Say { text: String, keep: Option<Keep> },
    /// Takes effect with the next turn: a turn already queued ahead of it is answered as the
    /// character it was asked of.
    Character(Character),
    /// Voiced in the current voice without asking the model.
    Greet(Greeting),
    /// Deliver one approved message. Answered by exactly one `relayed`.
    Relay(Order),
    /// A `relay` whose nonce can be read and whose other fields break a limit. It is still
    /// answered by one `relayed`, so the client is not left waiting for it.
    UnfitRelay { nonce: Nonce, why: String },
    Shutdown,
}

/// The line as it is written, before its fields are checked.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Wire {
    Say { text: String, record: Option<WireRecord> },
    Character { name: String, persona: String, voice: Option<String> },
    Greet { text: String },
    Relay { nonce: String, title: String, body: String, summary: String },
    Shutdown,
}

#[derive(Deserialize)]
struct WireRecord {
    to: String,
    via: String,
}

impl TryFrom<Wire> for Request {
    type Error = String;

    fn try_from(wire: Wire) -> Result<Request, String> {
        Ok(match wire {
            Wire::Say { text, record } => Request::Say { text, keep: record.map(|r| Keep::new(&r.to, &r.via)).transpose()? },
            Wire::Character { name, persona, voice } => Request::Character(Character::new(&name, &persona, voice.as_deref())?),
            Wire::Greet { text } => Request::Greet(Greeting::new(&text)?),
            Wire::Relay { nonce, title, body, summary } => {
                let nonce = Nonce::new(&nonce)?;
                match Order::new(nonce.clone(), &title, &body, &summary) {
                    Ok(order) => Request::Relay(order),
                    Err(why) => Request::UnfitRelay { nonce, why },
                }
            }
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
    /// She asked for a relay, in the user's words. Nothing she wrote this turn is said, and
    /// `turn_end` follows.
    RelayAsked { to: String, text: String },
    /// The courier moved on. `handing` carries the address and is sent before the message is
    /// given to the courier's model, so a client that loses the core after it knows a send may
    /// have begun.
    RelayStep {
        nonce: Nonce,
        #[serde(flatten)]
        step: relay::Step,
    },
    /// How one `relay` ended. `gated` is false only when a send succeeded that the gate did not let
    /// through.
    Relayed { nonce: Nonce, outcome: relay::Outcome, gated: bool, elapsed_ms: u64, cost_usd: Option<f64> },
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
