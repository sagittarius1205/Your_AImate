// The dialogue record: one JSON line per turn, appended to a file the client names on the `say`
// that starts the turn (Docs/DESIGN_ChatCore.md §13). The core never picks a place of its own and
// keeps nothing of a turn whose `say` did not ask, so a client that never asks, the VR side among
// them, gets no file. What a line holds is what was said and answered: never the prompt, a relay's
// message or the audio.

use crate::reply::Reply;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Raised when a field changes meaning, so a reader can tell the lines apart.
pub const VERSION: u32 = 1;

/// How the user's sentence reached the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    Mic,
    Text,
}

/// A `say`'s request to keep its turn: the file the line is appended to, and how the sentence came.
#[derive(Clone, Debug, PartialEq)]
pub struct Keep {
    pub to: PathBuf,
    pub via: Via,
}

impl Keep {
    pub fn new(to: &str, via: &str) -> Result<Keep, String> {
        if to.is_empty() {
            return Err("record.to is empty".to_string());
        }
        let via = match via {
            "mic" => Via::Mic,
            "text" => Via::Text,
            other => return Err(format!("record.via is mic or text, not {other:?}")),
        };
        Ok(Keep { to: PathBuf::from(to), via })
    }
}

/// A sentence as it reached the core, before the queue.
pub struct Heard {
    pub text: String,
    pub keep: Option<Keep>,
    pub arrived: Instant,
    /// Milliseconds since the Unix epoch, the unit `relay.jsonl` uses.
    pub at: u64,
}

impl Heard {
    pub fn now(text: String, keep: Option<Keep>) -> Heard {
        Heard { text, keep, arrived: Instant::now(), at: epoch_ms(SystemTime::now()) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Answered,
    /// She asked for a relay. Nothing she wrote was said, and what she asked to pass on is not kept.
    RelayAsked,
    /// The engine answered and the core turned the answer down.
    Refused,
    /// No answer the core could use came back.
    Failed,
    /// The core stopped before the turn ended. Which of it was heard cannot be told, so none of her
    /// answer is kept (Docs/DESIGN_ChatCore.md §3).
    Interrupted,
}

impl Outcome {
    /// Whether the engine heard the sentence and answered it, which is when a relay's note has
    /// reached her.
    pub fn answered(self) -> bool {
        !matches!(self, Outcome::Refused | Outcome::Failed)
    }
}

/// Her answer as the model wrote it: each clause's words, without its gesture.
#[derive(Debug, PartialEq, Serialize)]
pub struct Answer {
    pub clauses: Vec<String>,
    pub language: Option<String>,
    pub affect: Option<String>,
}

impl Answer {
    pub fn of(reply: &Reply) -> Answer {
        Answer {
            clauses: reply.clauses.iter().map(|c| c.text.clone()).collect(),
            language: reply.language.clone(),
            affect: reply.affect.clone(),
        }
    }

    /// The echo engine's, which says the sentence back as one clause.
    pub fn echo(text: &str) -> Answer {
        Answer { clauses: vec![text.to_string()], language: None, affect: None }
    }
}

/// What became of a turn that ran to its end.
pub struct Took {
    pub outcome: Outcome,
    pub answer: Option<Answer>,
    /// When the engine's answer, or its failure, came back.
    pub engine: Instant,
    /// When the first part went out, if one did.
    pub first_part: Option<Instant>,
    /// Seconds of voiced audio sent.
    pub audio: f64,
}

/// Which engine answered: the one that ran, which is not always the one asked for.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Who<'a> {
    pub kind: &'static str,
    pub model: Option<&'a str>,
}

/// Milliseconds from the sentence's arrival. `engine` and `first_audio` are left out when the turn
/// never got there, and `audio` goes with `first_audio`.
#[derive(Debug, PartialEq, Serialize)]
struct Ms {
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_audio: Option<u64>,
    done: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<u64>,
}

#[derive(Serialize)]
struct User<'a> {
    text: &'a str,
    via: Via,
}

#[derive(Serialize)]
struct Line<'a> {
    v: u32,
    at: u64,
    run: &'a str,
    turn: u64,
    user: User<'a>,
    reply: Option<&'a Answer>,
    outcome: Outcome,
    engine: Who<'a>,
    ms: Ms,
    /// From the end of her line before this one, a turn's or a greeting's, to this sentence's
    /// arrival. Below zero when the sentence came while that line was still being sent.
    gap_ms: Option<i64>,
}

/// The record's state for the life of one core.
pub struct Log {
    run: String,
    turns: u64,
    last_line: Option<Instant>,
    /// Set by a write that failed and cleared by one that worked, so an outage is said once.
    failing: bool,
}

impl Log {
    pub fn new() -> Log {
        Log { run: format!("{}-{}", epoch_ms(SystemTime::now()), std::process::id()), turns: 0, last_line: None, failing: false }
    }

    /// A greeting of hers just ended.
    pub fn greeted(&mut self) {
        self.last_line = Some(Instant::now());
    }

    /// A turn just ended, with `took` when it ran to its end. Counted whether or not it is kept, so
    /// a gap in `turn` is turns that were not recorded. Called after the turn's last event, so a
    /// write that is slow or fails never holds up what the user is waiting for.
    pub fn turn(&mut self, heard: &Heard, took: Option<&Took>, engine: Who) {
        self.turns += 1;
        let now = Instant::now();
        let gap_ms = self.last_line.map(|end| signed_ms(end, heard.arrived));
        self.last_line = Some(now);
        let Some(keep) = &heard.keep else { return };
        let since = |at: Instant| at.saturating_duration_since(heard.arrived).as_millis() as u64;
        let first_audio = took.and_then(|t| t.first_part).map(since);
        let line = Line {
            v: VERSION,
            at: heard.at,
            run: &self.run,
            turn: self.turns,
            user: User { text: &heard.text, via: keep.via },
            reply: took.and_then(|t| t.answer.as_ref()),
            outcome: took.map_or(Outcome::Interrupted, |t| t.outcome),
            engine,
            ms: Ms {
                engine: took.map(|t| since(t.engine)),
                first_audio,
                done: since(now),
                audio: first_audio.and(took).map(|t| (t.audio * 1000.0).round() as u64),
            },
            gap_ms,
        };
        match append(&keep.to, &line) {
            Ok(()) => self.failing = false,
            Err(why) => {
                if !self.failing {
                    eprintln!("vrchatcore: the dialogue record could not be written, and the turns go on without it until a write works again: {why}");
                }
                self.failing = true;
            }
        }
    }
}

/// One whole line, in one write. The folder is not made: the client names a file in a folder it
/// keeps, and one that has gone is a failure to note, not a place to recreate.
fn append(to: &Path, line: &Line) -> Result<(), String> {
    let mut text = serde_json::to_string(line).map_err(|e| format!("encoding the line: {e}"))?;
    text.push('\n');
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(to).map_err(|e| format!("{}: {e}", to.display()))?;
    file.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", to.display()))
}

fn epoch_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn signed_ms(from: Instant, to: Instant) -> i64 {
    match to.checked_duration_since(from) {
        Some(after) => after.as_millis() as i64,
        None => -(from.duration_since(to).as_millis() as i64),
    }
}
