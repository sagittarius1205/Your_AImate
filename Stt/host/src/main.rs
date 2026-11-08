//! `vrstt`: hold to talk, release, get text. Its own process, and it does not know the conversation
//! core exists (Docs/DESIGN_STT.md 0): a host presses and releases, and reads text back.
//!
//!   vrstt [run]            the wire on stdin and stdout (proto.rs)
//!   vrstt file <wav>...    each file is one press through the same engine, events on stdout
//!   vrstt probe [seconds]  opens the default microphone once through the real capture path
//!   vrstt packs            each pack this build knows, and whether it is installed and verifies
//!   vrstt selftest [name]  the headless tests, `SelfTest: PASS|FAIL <name>: <detail>`

#![deny(clippy::print_stdout)]

mod capture;
mod file;
mod pack;
mod proto;
mod selftest;
mod sherpa;
mod wav;

use std::io::BufRead;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::json;
use stt_core::engine::{Command, Engine, EngineConfig, Event, Events, Flow};

/// How often a live press drains the device: one frame.
const TICK: Duration = Duration::from_millis(20);

/// SenseVoice threads. Two leave the machine its other cores while the character is talking.
const ASR_THREADS: i32 = 2;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None | Some("run") => run(),
        Some("file") => files(&args[1..]),
        Some("probe") => probe(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1.0)),
        Some("selftest") => selftest::run(args.get(1).map(String::as_str)),
        Some("packs") => packs(),
        Some(other) => {
            eprintln!("vrstt: unknown subcommand {other:?}; expected run, file, probe, packs or selftest");
            2
        }
    };
    std::process::exit(code);
}

struct Stdout;

impl Events for Stdout {
    fn emit(&mut self, event: Event) {
        proto::send(&proto::event(&event));
    }
}

struct Loaded {
    asr: sherpa::SenseVoice,
    vad: sherpa::Silero,
    load_ms: u64,
}

/// Verifies every file against the manifest, then loads both models. A failure is one `error` line
/// with `kind: "model"` naming the file, so the host can say why in words.
fn load() -> Result<Loaded, String> {
    let began = Instant::now();
    let root = pack::default_root().ok_or("none of VRSTT_MODELS, VRPET_HOME or USERPROFILE is set")?;
    let manifest = pack::parse(pack::MANIFEST).map_err(|e| e.to_string())?;
    pack::check_wanted(&manifest, std::env::var(pack::PACK_ENV).ok().as_deref()).map_err(|e| e.to_string())?;
    let files = pack::verify(&manifest, &root).map_err(|e| e.to_string())?;
    let asr = sherpa::SenseVoice::load(&files, ASR_THREADS)?;
    let vad = sherpa::Silero::load(&files)?;
    Ok(Loaded { asr, vad, load_ms: began.elapsed().as_millis() as u64 })
}

/// One `packs` line: every pack this build knows, each with `ok`, and `why` when it is not installed
/// under the model root or does not match its hashes. A host offers only the ones that are `ok`.
fn packs() -> i32 {
    let manifest = match pack::parse(pack::MANIFEST) {
        Ok(m) => m,
        Err(e) => {
            proto::send(&proto::error(None, "model", &e.to_string()));
            return 3;
        }
    };
    let id = pack::pack_id(&manifest).unwrap_or("");
    let checked = pack::default_root()
        .ok_or_else(|| "none of VRSTT_MODELS, VRPET_HOME or USERPROFILE is set".to_string())
        .and_then(|root| pack::verify(&manifest, &root).map_err(|e| e.to_string()));
    let entry = match checked {
        Ok(_) => json!({ "id": id, "ok": true }),
        Err(why) => json!({ "id": id, "ok": false, "why": why }),
    };
    proto::send(&json!({ "ev": "packs", "packs": [entry] }));
    0
}

enum Input {
    Command(Command),
    Bad(String),
    Eof,
}

fn run() -> i32 {
    let loaded = match load() {
        Ok(l) => l,
        Err(why) => {
            proto::send(&proto::error(None, "model", &why));
            return 3;
        }
    };
    let format = capture::describe();
    proto::send(&proto::ready(sherpa::MODEL_NAME, sherpa::VAD_NAME, &format, loaded.load_ms));

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut stdin = std::io::stdin().lock();
        loop {
            buf.clear();
            let input = match stdin.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => Input::Eof,
                Ok(_) if buf.trim_ascii().is_empty() => continue,
                Ok(_) => proto::parse_command(buf.trim_ascii()).map_or_else(Input::Bad, Input::Command),
            };
            let eof = matches!(input, Input::Eof);
            if tx.send(input).is_err() || eof {
                return;
            }
        }
    });

    let mut engine =
        Engine::new(EngineConfig::default(), Box::new(capture::Mic), Box::new(loaded.asr), Box::new(loaded.vad), Box::new(Stdout));
    loop {
        let next = if engine.is_live() { rx.recv_timeout(TICK).ok() } else { rx.recv().ok().or(Some(Input::Eof)) };
        match next {
            Some(Input::Command(cmd)) => {
                if engine.apply(cmd) == Flow::Exit {
                    return 0;
                }
            }
            Some(Input::Bad(why)) => proto::send(&proto::error(None, "protocol", &why)),
            // The host is gone. A press it never released ends here, so the device closes with it.
            Some(Input::Eof) => {
                engine.apply(Command::Shutdown);
                return 0;
            }
            None => {}
        }
        engine.pump();
    }
}

fn files(paths: &[String]) -> i32 {
    if paths.is_empty() {
        eprintln!("vrstt file: name at least one WAV file");
        return 2;
    }
    let loaded = match load() {
        Ok(l) => l,
        Err(why) => {
            proto::send(&proto::error(None, "model", &why));
            return 3;
        }
    };
    proto::send(&json!({ "ev": "loaded", "load_ms": loaded.load_ms }));
    let capture = file::FileCapture::new();
    let (queue, done) = (capture.queue(), capture.done());
    let mut engine =
        Engine::new(EngineConfig::default(), Box::new(capture), Box::new(loaded.asr), Box::new(loaded.vad), Box::new(Stdout));
    for path in paths {
        let wav = match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| wav::read(&b)) {
            Ok(w) => w,
            Err(why) => {
                proto::send(&proto::error(None, "file", &format!("{path}: {why}")));
                continue;
            }
        };
        proto::send(&json!({
            "ev": "file",
            "path": path,
            "audio_ms": audio_ms(&wav),
            "native_rate": wav.format.rate,
            "native_channels": wav.format.channels,
        }));
        queue.borrow_mut().push_back(wav);
        engine.apply(Command::Press);
        while engine.is_live() && !done.get() {
            engine.pump();
        }
        engine.apply(Command::Release);
    }
    0
}

fn audio_ms(w: &wav::Wav) -> u64 {
    let n = match &w.samples {
        wav::Samples::I16(s) => s.len(),
        wav::Samples::F32(s) => s.len(),
    };
    n as u64 * 1000 / (u64::from(w.format.rate) * u64::from(w.format.channels))
}

fn probe(seconds: f64) -> i32 {
    let format = match capture::describe() {
        Ok(f) => f,
        Err(why) => {
            proto::send(&proto::error(None, "device", &why));
            return 4;
        }
    };
    let began = Instant::now();
    let mut stream = match stt_core::engine::Capture::open(&mut capture::Mic) {
        Ok(s) => s,
        Err(why) => {
            proto::send(&proto::error(None, "device", &why));
            return 4;
        }
    };
    let open_ms = began.elapsed().as_millis() as u64;
    let mut samples = 0usize;
    let mut first_audio_ms = None;
    let mut peak = 0i32;
    let mut dropped = 0u64;
    let mut glitches = 0u64;
    let until = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < until {
        std::thread::sleep(TICK);
        match stream.read() {
            Ok(chunk) => {
                if first_audio_ms.is_none() && !chunk.audio.is_empty() {
                    first_audio_ms = Some(began.elapsed().as_millis() as u64);
                }
                samples += chunk.audio.len();
                dropped += chunk.dropped;
                glitches += chunk.glitches;
                peak = peak.max(chunk.audio.samples().iter().map(|&s| i32::from(s).abs()).max().unwrap_or(0));
            }
            Err(why) => {
                proto::send(&proto::error(None, "device", &why));
                return 4;
            }
        }
    }
    drop(stream);
    proto::send(&json!({
        "ev": "probe",
        "capture": proto::capture_json(&format),
        "open_ms": open_ms,
        "first_audio_ms": first_audio_ms,
        "seconds": seconds,
        "samples_16k": samples,
        "peak": peak,
        "dropped_samples": dropped,
        "glitches": glitches,
    }));
    0
}
