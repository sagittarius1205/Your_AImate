//! The reducer (Docs/DESIGN_STT.md 4). One thread owns every piece of state, so there is no lock.
//! `live` is an `Option`, which is what "one capture at a time" is. The microphone, the recogniser,
//! the VAD and the event sink are traits, so the product and the self-tests run this same code.

use std::time::{Duration, Instant};

use crate::audio::Frames16k;
use crate::segment::{Heard, Segmenter, SegmenterConfig, Vad};

pub type SessionId = u32;

/// One utterance's text. An empty one cannot be built, so a press with no words cannot reach the
/// character as an empty sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript(String);

impl Transcript {
    pub fn new(text: &str) -> Option<Self> {
        let t = text.trim();
        (!t.is_empty()).then(|| Transcript(t.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Press,
    Release,
    Cancel,
    Shutdown,
}

/// Why a press produced no text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    NoSpeech,
    TooShort,
    /// The VAD heard speech and the recogniser returned no words.
    NoWords,
    Cancelled,
    /// The device went away mid-press. The `error` before this said which device and how.
    DeviceLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Device,
    Recognizer,
}

/// Measured per press. Wall-clock milliseconds except `speech_ms`, which is audio time, so a file
/// replayed faster than real time cannot pass for live latency (Docs/DESIGN_STT.md 7.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timings {
    pub open_ms: u64,
    pub held_ms: u64,
    pub speech_ms: u64,
    pub recognize_ms: u64,
    pub release_to_text_ms: u64,
    pub dropped_samples: u64,
    pub glitches: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Sent before the device is asked to open, so the host silences the character before the
    /// microphone can hear it (Docs/DESIGN_STT.md 5.1).
    CaptureStarted { session: SessionId },
    Transcript { session: SessionId, seq: u32, text: Transcript, timings: Timings },
    Nothing { session: SessionId, why: Why, timings: Timings },
    Failed { session: Option<SessionId>, kind: FailureKind, detail: String },
}

/// What one read of the device gave. `dropped` counts samples the ring had no room for, because a
/// silent truncation is the hardest fault to find.
pub struct Chunk {
    pub audio: Frames16k,
    pub dropped: u64,
    /// Over- or underruns the device reported. They lose an unknown number of samples, so they are
    /// counted apart from `dropped` and not added to it.
    pub glitches: u64,
}

/// An open device. Dropping it closes the device.
pub trait Stream {
    fn read(&mut self) -> Result<Chunk, String>;
}

pub trait Capture {
    fn open(&mut self) -> Result<Box<dyn Stream>, String>;
}

pub trait Recognizer {
    fn recognize(&mut self, speech: &Frames16k) -> Result<String, String>;
}

pub trait Events {
    fn emit(&mut self, event: Event);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineConfig {
    pub segmenter: SegmenterConfig,
    /// A press held longer than this is released by the engine itself. A stuck button must not
    /// leave the microphone open for good.
    pub max_press: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig { segmenter: SegmenterConfig::default(), max_press: Duration::from_secs(60) }
    }
}

struct Session {
    id: SessionId,
    pressed_at: Instant,
    stream: Box<dyn Stream>,
    seg: Segmenter,
    heard: Recognized,
}

/// The text so far and what it cost.
#[derive(Default)]
struct Recognized {
    texts: Vec<String>,
    timings: Timings,
}

pub struct Engine {
    cfg: EngineConfig,
    capture: Box<dyn Capture>,
    recognizer: Box<dyn Recognizer>,
    vad: Box<dyn Vad>,
    events: Box<dyn Events>,
    last_id: SessionId,
    live: Option<Session>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Exit,
}

impl Engine {
    pub fn new(
        cfg: EngineConfig,
        capture: Box<dyn Capture>,
        recognizer: Box<dyn Recognizer>,
        vad: Box<dyn Vad>,
        events: Box<dyn Events>,
    ) -> Self {
        Engine { cfg, capture, recognizer, vad, events, last_id: 0, live: None }
    }

    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// Every command reads the current state and applies only the difference, so a repeated one is
    /// a no-op: a second press while one is held, a release with nothing held.
    pub fn apply(&mut self, cmd: Command) -> Flow {
        match cmd {
            Command::Press => self.press(),
            Command::Release => self.release(),
            Command::Cancel => self.end_without_text(Why::Cancelled),
            Command::Shutdown => {
                self.end_without_text(Why::Cancelled);
                return Flow::Exit;
            }
        }
        Flow::Continue
    }

    fn press(&mut self) {
        if self.live.is_some() {
            return;
        }
        self.last_id += 1;
        let id = self.last_id;
        self.events.emit(Event::CaptureStarted { session: id });
        let pressed_at = Instant::now();
        match self.capture.open() {
            Ok(stream) => {
                self.vad.reset();
                let timings = Timings { open_ms: ms(pressed_at.elapsed()), ..Timings::default() };
                self.live = Some(Session {
                    id,
                    pressed_at,
                    stream,
                    seg: Segmenter::new(self.cfg.segmenter),
                    heard: Recognized { texts: Vec::new(), timings },
                });
            }
            Err(detail) => self.events.emit(Event::Failed { session: Some(id), kind: FailureKind::Device, detail }),
        }
    }

    /// Drains what the device has delivered. Called on a timer while a press is live.
    pub fn pump(&mut self) {
        self.drain();
        if self.live.as_ref().is_some_and(|s| s.pressed_at.elapsed() >= self.cfg.max_press) {
            self.release();
        }
    }

    fn drain(&mut self) {
        let Some(s) = self.live.as_mut() else { return };
        match s.stream.read() {
            Ok(chunk) => {
                s.heard.timings.dropped_samples += chunk.dropped;
                s.heard.timings.glitches += chunk.glitches;
                for segment in s.seg.push(&chunk.audio, self.vad.as_mut()) {
                    if let Err(detail) = recognize_into(self.recognizer.as_mut(), &mut s.heard, &segment) {
                        let id = s.id;
                        self.live = None;
                        self.events.emit(Event::Failed { session: Some(id), kind: FailureKind::Recognizer, detail });
                        return;
                    }
                }
            }
            Err(detail) => {
                let Some(s) = self.live.take() else { return };
                self.events.emit(Event::Failed { session: Some(s.id), kind: FailureKind::Device, detail });
                self.events.emit(Event::Nothing { session: s.id, why: Why::DeviceLost, timings: s.heard.timings });
            }
        }
    }

    fn release(&mut self) {
        self.drain();
        let Some(Session { id, pressed_at, stream, seg, mut heard }) = self.live.take() else { return };
        let released_at = Instant::now();
        heard.timings.held_ms = ms(released_at - pressed_at);
        drop(stream);

        let (last, speech) = seg.close();
        if let Some(segment) = last
            && let Err(detail) = recognize_into(self.recognizer.as_mut(), &mut heard, &segment)
        {
            self.events.emit(Event::Failed { session: Some(id), kind: FailureKind::Recognizer, detail });
            return;
        }

        let text = join(&heard.texts);
        let timings = Timings { release_to_text_ms: ms(released_at.elapsed()), ..heard.timings };
        let event = match (Transcript::new(&text), speech) {
            (Some(text), _) => Event::Transcript { session: id, seq: 0, text, timings },
            (None, Heard::Speech) => Event::Nothing { session: id, why: Why::NoWords, timings },
            (None, Heard::TooShort) => Event::Nothing { session: id, why: Why::TooShort, timings },
            (None, Heard::Nothing) => Event::Nothing { session: id, why: Why::NoSpeech, timings },
        };
        self.events.emit(event);
    }

    fn end_without_text(&mut self, why: Why) {
        if let Some(s) = self.live.take() {
            self.events.emit(Event::Nothing { session: s.id, why, timings: s.heard.timings });
        }
    }
}

fn recognize_into(rec: &mut dyn Recognizer, heard: &mut Recognized, segment: &Frames16k) -> Result<(), String> {
    let began = Instant::now();
    let text = rec.recognize(segment)?;
    heard.timings.recognize_ms += ms(began.elapsed());
    heard.timings.speech_ms += segment.ms();
    heard.texts.push(text);
    Ok(())
}

/// One press is one sentence however many pauses it had, so segments join into one text. A space
/// goes between two pieces only where both sides are ASCII words, which is how mixed Mandarin and
/// English is written.
fn join(texts: &[String]) -> String {
    let mut out = String::new();
    for t in texts.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        let ascii_seam = out.chars().last().is_some_and(|c| c.is_ascii_alphanumeric())
            && t.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
        if ascii_seam {
            out.push(' ');
        }
        out.push_str(t);
    }
    out
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}
