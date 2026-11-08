//! `vrstt selftest [name]`: the headless tests (Docs/DESIGN_STT.md 6) for what this slice holds.
//! One line per test, `SelfTest: PASS|FAIL <name>: <detail>`, the shape the pet's own tests print.
//! A test that ran no checks fails, and an unknown name fails with the list of known ones.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use serde_json::{Value, json};
use stt_core::audio::{Boundary, CaptureFormat, FRAME, Frames16k, NativeFormat, Pcm, SAMPLE_RATE};
use stt_core::engine::{
    Capture, Chunk, Command, Engine, EngineConfig, Event, Events, FailureKind, Recognizer, Stream, Timings, Transcript,
    Why,
};
use stt_core::segment::{Heard, Segmenter, SegmenterConfig, Vad};

use crate::{pack, proto, wav};

struct Checks {
    run: usize,
    failures: Vec<String>,
    /// Measured values, printed on the result line so a pass still says by how much.
    notes: Vec<String>,
}

impl Checks {
    fn note(&mut self, what: impl Into<String>) {
        self.notes.push(what.into());
    }

    fn that(&mut self, ok: bool, what: impl Into<String>) {
        self.run += 1;
        if !ok {
            self.failures.push(what.into());
        }
    }

    fn eq<T: PartialEq + Debug>(&mut self, got: T, want: T, what: impl Into<String>) {
        self.run += 1;
        if got != want {
            self.failures.push(format!("{}: expected {want:?}, saw {got:?}", what.into()));
        }
    }
}

type Case = (&'static str, fn(&mut Checks));

const TESTS: &[Case] = &[
    ("segmenter", segmenter),
    ("resample_once", resample_once),
    ("one_capture", one_capture),
    ("manifest", manifest),
    ("wire", wire),
    ("stdout_is_json", stdout_is_json),
];

pub fn run(only: Option<&str>) -> i32 {
    let chosen: Vec<_> = match only {
        None => TESTS.iter().collect(),
        Some(name) => {
            let found: Vec<_> = TESTS.iter().filter(|(n, _)| *n == name).collect();
            if found.is_empty() {
                let known: Vec<&str> = TESTS.iter().map(|(n, _)| *n).collect();
                proto::line(&format!("SelfTest: FAIL selftest: unknown test `{name}`; known: {}", known.join(", ")));
                return 1;
            }
            found
        }
    };
    let mut failed = 0;
    for (name, body) in chosen {
        let mut c = Checks { run: 0, failures: Vec::new(), notes: Vec::new() };
        if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| body(&mut c))) {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            c.failures.push(format!("panicked: {msg}"));
        }
        if c.run == 0 {
            c.failures.push("ran no checks, so it proves nothing".to_string());
        }
        if c.failures.is_empty() {
            let notes = if c.notes.is_empty() { String::new() } else { format!(" ({})", c.notes.join(", ")) };
            proto::line(&format!("SelfTest: PASS {name}: {} checks{notes}", c.run));
        } else {
            failed += 1;
            let shown: Vec<String> = c.failures.iter().take(4).cloned().collect();
            let more = c.failures.len().saturating_sub(4);
            let tail = if more > 0 { format!(" | +{more} more") } else { String::new() };
            proto::line(&format!("SelfTest: FAIL {name}: {}{tail}", shown.join(" | ").replace(['\r', '\n'], " ")));
        }
    }
    i32::from(failed > 0)
}

// ---------------------------------------------------------------------------------------------
// Scripted audio. Speech is a frame of a constant non-zero value and silence is zeros, and the VAD
// reads that back, so the mask is the audio and every boundary is exact to the frame.

const LOUD: i16 = 1000;

struct MaskVad;

impl Vad for MaskVad {
    fn voiced(&mut self, frame: &[i16]) -> bool {
        frame[0] != 0
    }

    fn reset(&mut self) {}
}

/// `(voiced, milliseconds)` runs as 16 kHz audio, built through a 16 kHz boundary so the test needs
/// no constructor the product does not have.
fn script(runs: &[(bool, u32)]) -> Frames16k {
    let mut pcm = Vec::new();
    for &(voiced, ms) in runs {
        pcm.extend(std::iter::repeat_n(if voiced { LOUD } else { 0 }, (ms * SAMPLE_RATE / 1000) as usize));
    }
    passthrough(&pcm)
}

fn passthrough(pcm: &[i16]) -> Frames16k {
    let mut b = Boundary::new(NativeFormat { rate: SAMPLE_RATE, channels: 1 }).expect("16 kHz mono is a format");
    b.push(Pcm::I16(pcm))
}

fn segment_all(cfg: SegmenterConfig, audio: &Frames16k, chunk: usize) -> (Vec<Frames16k>, Heard) {
    let mut seg = Segmenter::new(cfg);
    let mut out = Vec::new();
    for piece in audio.samples().chunks(chunk.max(1)) {
        out.extend(seg.push(&passthrough(piece), &mut MaskVad));
    }
    let (last, heard) = seg.close();
    out.extend(last);
    (out, heard)
}

fn ms_of(f: &Frames16k) -> u64 {
    f.ms()
}

fn loud_ms(f: &Frames16k) -> u64 {
    f.samples().iter().filter(|&&s| s != 0).count() as u64 * 1000 / u64::from(SAMPLE_RATE)
}

fn segmenter(c: &mut Checks) {
    let cfg = SegmenterConfig::default();

    // Leading and trailing silence trimmed to the pads: 300 ms before, 200 ms after.
    let (segs, heard) = segment_all(cfg, &script(&[(false, 1000), (true, 1000), (false, 1000)]), 777);
    c.eq(segs.len(), 1, "one second of speech in silence is one segment");
    c.eq(heard, Heard::Speech, "and the press heard speech");
    if let Some(s) = segs.first() {
        c.eq(ms_of(s), 300 + 1000 + 200, "trimmed to pre pad + speech + post pad");
        c.eq(loud_ms(s), 1000, "with all of the speech in it");
        c.that(s.samples()[..300 * 16].iter().all(|&x| x == 0), "the pre pad is the silence before");
    }

    // A pause of exactly the split length cuts; one frame shorter does not.
    let (segs, _) = segment_all(cfg, &script(&[(true, 500), (false, 700), (true, 500)]), FRAME);
    c.eq(segs.len(), 2, "a 700 ms pause splits the press in two");
    let (segs, _) = segment_all(cfg, &script(&[(true, 500), (false, 680), (true, 500)]), FRAME);
    c.eq(segs.len(), 1, "a 680 ms pause does not");
    if let Some(s) = segs.first() {
        c.eq(loud_ms(s), 1000, "and the one segment holds both halves");
    }

    // Under the minimum is no segment, and says so; nothing voiced is a different answer.
    let (segs, heard) = segment_all(cfg, &script(&[(false, 300), (true, 140), (false, 300)]), FRAME);
    c.eq(segs.len(), 0, "140 ms of speech is no segment");
    c.eq(heard, Heard::TooShort, "and the press is too short, not silent");
    let (segs, heard) = segment_all(cfg, &script(&[(false, 300), (true, 160), (false, 300)]), FRAME);
    c.eq(segs.len(), 1, "160 ms of speech is a segment");
    c.eq(heard, Heard::Speech, "and counts as speech");
    let (segs, heard) = segment_all(cfg, &script(&[(false, 2000)]), 1000);
    c.eq(segs.len(), 0, "silence is no segment");
    c.eq(heard, Heard::Nothing, "and the press heard nothing");

    // A stuck stream of speech is cut at the maximum, and none of it is lost.
    let (segs, _) = segment_all(cfg, &script(&[(true, 40_000)]), 16_000);
    c.eq(segs.iter().map(ms_of).collect::<Vec<_>>(), vec![30_000, 10_000], "40 s of speech is 30 s then 10 s");

    // Speech that starts in the first frame has no silence to pad with, and is not delayed.
    let (segs, _) = segment_all(cfg, &script(&[(true, 400)]), 123);
    c.eq(segs.first().map(ms_of), Some(400), "speech from the first frame, released at once, is all kept");

    // How the audio arrives must not change where the cuts fall.
    let audio = script(&[(false, 450), (true, 620), (false, 900), (true, 260), (false, 120)]);
    let a: Vec<usize> = segment_all(cfg, &audio, 1).0.iter().map(Frames16k::len).collect();
    let b: Vec<usize> = segment_all(cfg, &audio, 4096).0.iter().map(Frames16k::len).collect();
    c.eq(a, b, "one sample at a time and 4096 at a time cut the same");
}

// ---------------------------------------------------------------------------------------------
// resample_once (Docs/DESIGN_STT.md 1.1)

fn tone(rate: u32, hz: f64, seconds: f64, amp: f64) -> Vec<f32> {
    let n = (f64::from(rate) * seconds) as usize;
    (0..n).map(|i| (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin()) as f32).collect()
}

/// Power at `hz` against the power everywhere else, from a Hann-windowed DFT over the middle of the
/// signal (the ends carry the filter's start-up).
fn spectrum(samples: &[i16]) -> Vec<f64> {
    let mid = &samples[samples.len() / 4..samples.len() / 4 + 8000];
    let n = mid.len();
    let x: Vec<f64> = mid
        .iter()
        .enumerate()
        .map(|(i, &s)| f64::from(s) * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()))
        .collect();
    (0..=n / 2)
        .map(|k| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, v) in x.iter().enumerate() {
                let a = 2.0 * std::f64::consts::PI * (k * i) as f64 / n as f64;
                re += v * a.cos();
                im -= v * a.sin();
            }
            re * re + im * im
        })
        .collect()
}

fn resample(format: NativeFormat, interleaved: &[f32], chunk: usize) -> (Vec<i16>, bool) {
    let mut b = Boundary::new(format).expect("a valid format");
    let mut out = Vec::new();
    for piece in interleaved.chunks(chunk.max(1)) {
        out.extend_from_slice(b.push(Pcm::F32(piece)).samples());
    }
    out.extend_from_slice(b.finish().samples());
    (out, b.resampled())
}

fn resample_once(c: &mut Checks) {
    // 16 kHz mono i16 passes through untouched, to the bit.
    let pcm: Vec<i16> = (0..16_000i32).map(|i| ((i * 7919) % 65_536 - 32_768) as i16).collect();
    let mut b = match Boundary::new(NativeFormat { rate: 16_000, channels: 1 }) {
        Ok(b) => b,
        Err(e) => return c.that(false, format!("16 kHz mono refused: {e}")),
    };
    c.that(!b.resampled(), "a 16 kHz device is not resampled");
    let mut out = Vec::new();
    for piece in pcm.chunks(333) {
        out.extend_from_slice(b.push(Pcm::I16(piece)).samples());
    }
    out.extend_from_slice(b.finish().samples());
    c.that(out == pcm, "16 kHz mono i16 comes out bit for bit");

    // 48 kHz: 1 kHz stays at 1 kHz, the length is a third, and nothing else rises above -60 dB.
    let fmt48 = NativeFormat { rate: 48_000, channels: 1 };
    let (out, resampled) = resample(fmt48, &tone(48_000, 1000.0, 1.0, 0.5), 480);
    c.that(resampled, "a 48 kHz device reports resampled");
    c.that((out.len() as i64 - 16_000).abs() <= 1, format!("one second at 48 kHz is 16000 samples, got {}", out.len()));
    let p = spectrum(&out);
    let peak = (0..p.len()).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap_or(0);
    c.eq(peak, 1000 * 8000 / 16_000, "the peak is the 1 kHz bin");
    let signal: f64 = p[peak.saturating_sub(3)..=peak + 3].iter().sum();
    let rest: f64 = p.iter().sum::<f64>() - signal;
    let snr = 10.0 * (signal / rest.max(1e-30)).log10();
    c.that(snr > 60.0, format!("everything but the tone is more than 60 dB down, measured {snr:.1} dB"));
    c.note(format!("1 kHz at 48 kHz: rest {:.1} dB", -snr));

    // A 12 kHz tone is above the new Nyquist and would fold to 4 kHz. It must be gone.
    let (folded, _) = resample(fmt48, &tone(48_000, 12_000.0, 1.0, 0.5), 480);
    let (kept, _) = resample(fmt48, &tone(48_000, 1000.0, 1.0, 0.5), 480);
    // The middle half only: a tone switched on at sample 0 is a click, and a click is broadband.
    let energy = |v: &[i16]| v[v.len() / 4..v.len() * 3 / 4].iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>();
    let image_db = 10.0 * (energy(&folded) / energy(&kept)).log10();
    c.that(image_db < -60.0, format!("a 12 kHz tone leaves under -60 dB of image, measured {image_db:.1} dB"));
    c.note(format!("12 kHz image {image_db:.1} dB"));

    // 44.1 kHz is not an integer ratio and must still land on 1 kHz.
    let (out, _) = resample(NativeFormat { rate: 44_100, channels: 1 }, &tone(44_100, 1000.0, 1.0, 0.5), 441);
    let p = spectrum(&out);
    let peak = (0..p.len()).max_by(|&a, &b| p[a].total_cmp(&p[b])).unwrap_or(0);
    c.eq(peak, 500, "44.1 kHz: the peak is still the 1 kHz bin");

    // Stereo is averaged to mono, and a push that splits a frame between channels loses nothing.
    let mono = tone(48_000, 1000.0, 0.5, 0.5);
    let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
    let fmt_st = NativeFormat { rate: 48_000, channels: 2 };
    let (a, _) = resample(fmt_st, &stereo, 7);
    let (b, _) = resample(fmt48, &mono, 480);
    c.that(a == b, "two identical channels resample to the same as one");

    // Chunking must not change a sample: the filter carries its history across pushes.
    let (one, _) = resample(fmt48, &tone(48_000, 440.0, 0.3, 0.4), 14_400);
    let (many, _) = resample(fmt48, &tone(48_000, 440.0, 0.3, 0.4), 37);
    c.that(one == many, "one push and many pushes give the same samples");

    c.that(Boundary::new(NativeFormat { rate: 48_000, channels: 0 }).is_err(), "zero channels is refused");
    c.that(Boundary::new(NativeFormat { rate: 1_000, channels: 1 }).is_err(), "1 kHz is refused");
}

// ---------------------------------------------------------------------------------------------
// one_capture: the engine with a scripted device, recogniser and sink.

type Log = Rc<RefCell<Vec<Event>>>;

struct Recorder(Log);

impl Events for Recorder {
    fn emit(&mut self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

#[derive(Default)]
struct DeviceLog {
    opens: usize,
    open_now: usize,
    /// How many events had been emitted when each open happened.
    events_at_open: Vec<usize>,
}

struct ScriptedCapture {
    log: Rc<RefCell<DeviceLog>>,
    events: Log,
    /// What each open does: `Err` fails it, `Ok` plays that audio and then fails if the flag says so.
    plans: VecDeque<Result<(Frames16k, bool), String>>,
}

impl Capture for ScriptedCapture {
    fn open(&mut self) -> Result<Box<dyn Stream>, String> {
        let plan = self.plans.pop_front().unwrap_or_else(|| Ok((Frames16k::default(), false)));
        let mut log = self.log.borrow_mut();
        log.events_at_open.push(self.events.borrow().len());
        let (audio, lose) = plan?;
        log.opens += 1;
        log.open_now += 1;
        Ok(Box::new(ScriptedStream { audio: Some(audio), lose, log: Rc::clone(&self.log) }))
    }
}

struct ScriptedStream {
    audio: Option<Frames16k>,
    lose: bool,
    log: Rc<RefCell<DeviceLog>>,
}

impl Stream for ScriptedStream {
    fn read(&mut self) -> Result<Chunk, String> {
        match self.audio.take() {
            Some(audio) => Ok(Chunk { audio, dropped: 0, glitches: 0 }),
            None if self.lose => Err("the scripted device was unplugged".to_string()),
            None => Ok(Chunk { audio: Frames16k::default(), dropped: 0, glitches: 0 }),
        }
    }
}

impl Drop for ScriptedStream {
    fn drop(&mut self) {
        self.log.borrow_mut().open_now -= 1;
    }
}

/// Names each segment by its loud milliseconds, so a test can see which speech became which text.
struct Namer;

impl Recognizer for Namer {
    fn recognize(&mut self, speech: &Frames16k) -> Result<String, String> {
        Ok(match loud_ms(speech) {
            0 => String::new(),
            ms => format!("s{ms}"),
        })
    }
}

fn rig(plans: Vec<Result<(Frames16k, bool), String>>) -> (Engine, Log, Rc<RefCell<DeviceLog>>) {
    let events: Log = Rc::default();
    let log: Rc<RefCell<DeviceLog>> = Rc::default();
    let capture = ScriptedCapture { log: Rc::clone(&log), events: Rc::clone(&events), plans: plans.into() };
    let engine = Engine::new(
        EngineConfig::default(),
        Box::new(capture),
        Box::new(Namer),
        Box::new(MaskVad),
        Box::new(Recorder(Rc::clone(&events))),
    );
    (engine, events, log)
}

fn kinds(events: &Log) -> Vec<String> {
    events
        .borrow()
        .iter()
        .map(|e| match e {
            Event::CaptureStarted { session } => format!("started {session}"),
            Event::Transcript { session, text, .. } => format!("text {session} {}", text.as_str()),
            Event::Nothing { session, why, .. } => format!("nothing {session} {why:?}"),
            Event::Failed { session, kind, .. } => format!("failed {session:?} {kind:?}"),
        })
        .collect()
}

fn one_capture(c: &mut Checks) {
    let speech = || script(&[(false, 200), (true, 600), (false, 900), (true, 400), (false, 200)]);

    // Two presses and a release are one capture: one start, one open, one result.
    let (mut e, events, log) = rig(vec![Ok((speech(), false)), Ok((speech(), false))]);
    e.apply(Command::Press);
    e.apply(Command::Press);
    e.pump();
    e.apply(Command::Release);
    c.eq(kinds(&events), vec!["started 1".to_string(), "text 1 s600 s400".to_string()], "press twice, release once");
    c.eq(log.borrow().opens, 1, "the device opened once");
    c.eq(log.borrow().open_now, 0, "and is closed after the release");
    c.eq(log.borrow().events_at_open.clone(), vec![1], "capture_started was out before the device was asked to open");

    // A release with nothing held, and a second release, are nothing at all.
    let before = events.borrow().len();
    e.apply(Command::Release);
    e.apply(Command::Release);
    c.eq(events.borrow().len(), before, "a release with no press emits nothing");
    e.apply(Command::Press);
    e.apply(Command::Release);
    c.eq(kinds(&events).last().cloned(), Some("text 2 s600 s400".to_string()), "the next press is a new session");

    // Silence, a blip, and a cancel each end with their own reason and no text.
    let (mut e, events, _) = rig(vec![
        Ok((script(&[(false, 800)]), false)),
        Ok((script(&[(false, 300), (true, 100), (false, 300)]), false)),
        Ok((speech(), false)),
    ]);
    for _ in 0..2 {
        e.apply(Command::Press);
        e.apply(Command::Release);
    }
    e.apply(Command::Press);
    e.apply(Command::Cancel);
    c.eq(
        kinds(&events),
        ["started 1", "nothing 1 NoSpeech", "started 2", "nothing 2 TooShort", "started 3", "nothing 3 Cancelled"]
            .map(String::from)
            .to_vec(),
        "silence, a 100 ms blip and a cancel",
    );

    // A device that will not open says so, holds no session, and the next press opens one.
    let (mut e, events, log) = rig(vec![Err("busy".to_string()), Ok((speech(), false))]);
    e.apply(Command::Press);
    c.that(!e.is_live(), "a failed open leaves no live press");
    e.apply(Command::Press);
    e.apply(Command::Release);
    c.eq(
        kinds(&events),
        ["started 1", "failed Some(1) Device", "started 2", "text 2 s600 s400"].map(String::from).to_vec(),
        "a failed open, then a working one",
    );
    c.eq(log.borrow().open_now, 0, "no device is left open");

    // A device lost mid-press ends the press with no text and frees it for the next.
    let (mut e, events, log) = rig(vec![Ok((speech(), true)), Ok((speech(), false))]);
    e.apply(Command::Press);
    e.pump();
    e.pump();
    c.that(!e.is_live(), "a lost device ends the press");
    c.eq(log.borrow().open_now, 0, "and its stream is closed");
    e.apply(Command::Release);
    e.apply(Command::Press);
    e.apply(Command::Release);
    c.eq(
        kinds(&events),
        ["started 1", "failed Some(1) Device", "nothing 1 DeviceLost", "started 2", "text 2 s600 s400"].map(String::from).to_vec(),
        "lost, then a fresh press",
    );

    // A shutdown with a press held ends it before exiting.
    let (mut e, events, log) = rig(vec![Ok((speech(), false))]);
    e.apply(Command::Press);
    c.eq(e.apply(Command::Shutdown), stt_core::engine::Flow::Exit, "shutdown exits");
    c.eq(kinds(&events).last().cloned(), Some("nothing 1 Cancelled".to_string()), "and ends the held press");
    c.eq(log.borrow().open_now, 0, "with the device closed");

    c.that(Transcript::new("  \n ").is_none(), "an all-blank transcript cannot be built");
}

// ---------------------------------------------------------------------------------------------
// manifest

fn manifest(c: &mut Checks) {
    match pack::parse(pack::MANIFEST) {
        Ok(m) => {
            let roles: Vec<pack::Role> = m.assets.iter().map(|a| a.role).collect();
            c.eq(roles, vec![pack::Role::AsrModel, pack::Role::AsrTokens, pack::Role::Vad], "the checked-in manifest names three files");
            c.that(m.assets.iter().all(|a| a.sha256.len() == 64), "each with a full SHA-256");
        }
        Err(e) => c.that(false, format!("the checked-in manifest does not parse: {e}")),
    }

    let dir = std::env::temp_dir().join(format!("vrstt-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let files = [("m/model.onnx", b"model bytes".as_slice()), ("m/tokens.txt", b"tokens"), ("vad.onnx", b"vad")];
    for (path, bytes) in files {
        let p = dir.join(path);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&p, bytes);
    }
    let hash = |p: &str| pack::sha256_file(&dir.join(p)).unwrap_or_default();
    let manifest_text = |model_sha: &str, model_path: &str, version: u32| {
        json!({
            "version": version,
            "assets": [
                { "role": "asr_model", "path": model_path, "bytes": 11, "sha256": model_sha },
                { "role": "asr_tokens", "path": "m/tokens.txt", "bytes": 6, "sha256": hash("m/tokens.txt") },
                { "role": "vad", "path": "vad.onnx", "bytes": 3, "sha256": hash("vad.onnx") },
            ]
        })
        .to_string()
    };
    let good = manifest_text(&hash("m/model.onnx"), "m/model.onnx", 1);
    let verified = pack::parse(&good).and_then(|m| pack::verify(&m, &dir));
    c.that(verified.is_ok(), format!("matching files verify: {verified:?}"));

    let wrong = "0".repeat(64);
    match pack::parse(&manifest_text(&wrong, "m/model.onnx", 1)).and_then(|m| pack::verify(&m, &dir)) {
        Err(pack::PackError::Checksum { file, .. }) => {
            c.that(file.ends_with("m/model.onnx"), format!("a hash mismatch names the file: {}", file.display()))
        }
        other => c.that(false, format!("a hash mismatch is refused as Checksum, got {other:?}")),
    }
    let _ = std::fs::write(dir.join("m/model.onnx"), b"model bytez");
    match pack::parse(&good).and_then(|m| pack::verify(&m, &dir)) {
        Err(pack::PackError::Checksum { .. }) => c.that(true, "one changed byte is refused"),
        other => c.that(false, format!("one changed byte is refused as Checksum, got {other:?}")),
    }
    let _ = std::fs::write(dir.join("m/model.onnx"), b"model");
    c.that(
        matches!(pack::parse(&good).and_then(|m| pack::verify(&m, &dir)), Err(pack::PackError::Size { .. })),
        "a short file is refused by size before it is hashed",
    );
    let _ = std::fs::remove_file(dir.join("m/model.onnx"));
    c.that(
        matches!(pack::parse(&good).and_then(|m| pack::verify(&m, &dir)), Err(pack::PackError::Missing { .. })),
        "a missing file is refused as Missing",
    );
    c.that(
        matches!(pack::parse(&manifest_text(&wrong, "../escape.onnx", 1)), Err(pack::PackError::PathEscapes { .. })),
        "a path that climbs out is refused",
    );
    c.that(
        matches!(pack::parse(&manifest_text(&wrong, "m/model.onnx", 2)), Err(pack::PackError::UnknownVersion { found: 2 })),
        "an unknown manifest version is refused",
    );
    c.eq(pack::parse(&good).ok().and_then(|m| pack::pack_id(&m).map(str::to_string)), None, "a model outside packs/ names no pack");
    let _ = std::fs::remove_dir_all(&dir);

    // The pack a host chose: the checked-in one, by the folder its model is in, and nothing else.
    match pack::parse(pack::MANIFEST) {
        Ok(m) => {
            c.eq(pack::pack_id(&m), Some("sense-voice-zh-en-ja-ko-yue-int8-2024-07-17"), "the manifest's pack is named by its folder");
            c.that(pack::check_wanted(&m, None).is_ok(), "no pack asked for is the manifest's own");
            c.that(pack::check_wanted(&m, Some("sense-voice-zh-en-ja-ko-yue-int8-2024-07-17")).is_ok(), "and asking for it by name loads it");
            match pack::check_wanted(&m, Some("whisper-large")) {
                Err(e @ pack::PackError::UnknownPack { .. }) => {
                    c.that(e.to_string().contains("whisper-large"), format!("another pack is refused by name: {e}"))
                }
                other => c.that(false, format!("another pack is refused as UnknownPack, got {other:?}")),
            }
        }
        Err(e) => c.that(false, format!("the checked-in manifest does not parse: {e}")),
    }
}

// ---------------------------------------------------------------------------------------------
// wire

fn wire(c: &mut Checks) {
    for (line, want) in [
        (r#"{"op":"press"}"#, Command::Press),
        (r#"{"op":"release"}"#, Command::Release),
        (r#"{"op":"cancel"}"#, Command::Cancel),
        (r#"{"op":"shutdown","extra":1}"#, Command::Shutdown),
    ] {
        c.eq(proto::parse_command(line.as_bytes()), Ok(want), format!("{line} parses"));
    }
    for (line, says) in [
        (r#"{"op":"jump"}"#.as_bytes(), "unknown op"),
        (br#"{"op":1}"#.as_slice(), "no string"),
        (b"{not json".as_slice(), "not JSON"),
        (b"\xff\xfe".as_slice(), "not UTF-8"),
    ] {
        let r = proto::parse_command(line);
        c.that(r.as_ref().is_err_and(|e| e.contains(says)), format!("{:?} is refused saying {says:?}: {r:?}", String::from_utf8_lossy(line)));
    }

    let t = Timings { open_ms: 31, held_ms: 2000, speech_ms: 1500, recognize_ms: 120, release_to_text_ms: 90, dropped_samples: 0, glitches: 1 };
    let tj = json!({"open_ms":31,"held_ms":2000,"speech_ms":1500,"recognize_ms":120,"release_to_text_ms":90,"dropped_samples":0,"glitches":1});
    let text = Transcript::new("你好 world").unwrap_or_else(|| unreachable!());
    for (event, want) in [
        (Event::CaptureStarted { session: 3 }, json!({"ev":"capture_started","session":3})),
        (
            Event::Transcript { session: 3, seq: 0, text, timings: t },
            json!({"ev":"transcript","session":3,"seq":0,"text":"你好 world","timings":tj}),
        ),
        (Event::Nothing { session: 4, why: Why::TooShort, timings: t }, json!({"ev":"nothing","session":4,"why":"too_short","timings":tj})),
        (
            Event::Failed { session: Some(5), kind: FailureKind::Device, detail: "gone".into() },
            json!({"ev":"error","session":5,"kind":"device","message":"gone"}),
        ),
    ] {
        c.eq(proto::event(&event), want, "an event's wire shape");
    }
    for (why, name) in [
        (Why::NoSpeech, "no_speech"),
        (Why::TooShort, "too_short"),
        (Why::NoWords, "no_words"),
        (Why::Cancelled, "cancelled"),
        (Why::DeviceLost, "device_lost"),
    ] {
        c.eq(proto::why_name(why), name, "a reason's wire name");
    }

    let fmt = CaptureFormat { device: "Mic".into(), native: NativeFormat { rate: 48_000, channels: 2 }, resampled: true };
    c.eq(
        proto::ready("m", "v", &Ok(fmt), 7),
        json!({"ev":"ready","version":1,"model":"m","vad":"v","load_ms":7,
               "capture":{"device":"Mic","native_rate":48000,"native_channels":2,"resampled":true}}),
        "ready with a device",
    );
    c.eq(
        proto::ready("m", "v", &Err("no default input device".into()), 7),
        json!({"ev":"ready","version":1,"model":"m","vad":"v","load_ms":7,"capture":null,"capture_error":"no default input device"}),
        "ready without one says why",
    );
}

/// Runs this very executable in `file` mode on a generated WAV and reads its stdout: every line has
/// to be one JSON object with an `ev`. sherpa-onnx logs from C++, and a log line on stdout would
/// break every host, so this runs the real models when they are installed.
fn stdout_is_json(c: &mut Checks) {
    let Ok(exe) = std::env::current_exe() else { return c.that(false, "no current_exe") };
    let path = std::env::temp_dir().join(format!("vrstt-selftest-{}.wav", std::process::id()));
    let pcm: Vec<i16> = tone(16_000, 300.0, 1.0, 0.2).into_iter().map(|s| (s * 32767.0) as i16).collect();
    if std::fs::write(&path, wav::write_pcm16(16_000, &pcm)).is_err() {
        return c.that(false, "could not write the WAV");
    }
    let out = std::process::Command::new(exe).arg("file").arg(&path).output();
    let _ = std::fs::remove_file(&path);
    let Ok(out) = out else { return c.that(false, "could not run vrstt file") };
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    c.that(!lines.is_empty(), "vrstt file printed something");
    for l in &lines {
        let ok = serde_json::from_str::<Value>(l).ok().is_some_and(|v| v.get("ev").and_then(Value::as_str).is_some());
        c.that(ok, format!("stdout line is a JSON event: {l}"));
    }

    // `packs` against an empty model root: one line, the pack named, not ok, and why.
    let Ok(exe) = std::env::current_exe() else { return c.that(false, "no current_exe") };
    let empty = std::env::temp_dir().join(format!("vrstt-selftest-empty-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&empty);
    let out = std::process::Command::new(exe).arg("packs").env(pack::ROOT_ENV, &empty).output();
    let _ = std::fs::remove_dir_all(&empty);
    let Ok(out) = out else { return c.that(false, "could not run vrstt packs") };
    let line: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    c.eq(line.get("ev"), Some(&json!("packs")), "vrstt packs prints one packs line");
    c.eq(line.pointer("/packs/0/ok"), Some(&json!(false)), "a pack not installed is not ok");
    c.that(
        line.pointer("/packs/0/why").and_then(Value::as_str).is_some_and(|w| w.contains("missing")),
        format!("and says why: {line}"),
    );
}
