//! The pre-rendered thinking fillers, and the one rule that picks which one plays.
//!
//! Why these exist. The character is silent for a measured 7 to 14 seconds after the user speaks:
//! the LLM turn alone is 3 to 10 s and synthesising the whole reply is about 4 s on top
//! (Docs/DESIGN_ChatCore.md section 11). Changing the seam was measured and buys about one second
//! of the smaller term while opening a four-second hole mid-sentence, so the first thing that
//! covers the silence is a short vocalisation played at once, before the core has said anything.
//!
//! These are real VoxCPM2 takes in the frozen product voice, so the envelope below is measured
//! from real PCM. That matters: Docs/DESIGN_Voice.md:155 forbids faking mouth movement from
//! character counts, and a pre-rendered take is not that. The page drives the mouth from this
//! envelope through the same path real speech will use.
//!
//! The bank is compiled in rather than read from disk. It is 340 KiB of text, it ships atomically
//! with the code that indexes it, and a bank read at run time would add a missing-file state to a
//! feature whose whole job is to make the character respond immediately. A character with fillers in
//! her own voice is the exception, because her bank arrives with her folder and not with the build
//! (`Bank`), and when hers cannot be read the rule falls back to an answer that is always there.

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// One filler, in the shape the page receives.
///
/// `pcm_b64` is 16 kHz mono PCM16, little-endian, base64. It is base64 in the file so that nothing
/// here encodes or decodes anything: the string is parsed out and handed on. `save_shot` in
/// main.rs already moves audio-sized data over this IPC as base64, so this is the pattern this app
/// already has.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Filler {
    pub name: String,
    pub text: String,
    pub secs: f32,
    pub samples: usize,
    /// One loudness value in 0..1 per [`FRAME_SAMPLES`], gated at the solver's own silence floor.
    /// Loudness and not a solved mouth weight, because `cue/beats.rs` reads the same envelope to
    /// find prosodic cues (Docs/DESIGN_Pet.md:408) and a solved weight is no use to it.
    pub envelope: Vec<f32>,
    pub pcm_b64: String,
    /// What Whisper heard, and the character error rate against `text`. Kept so that a take that
    /// drifts is visible in the artifact rather than only in the run that rendered it.
    pub heard: String,
    pub cer: f32,
}

#[derive(Debug, Deserialize)]
struct BankFile {
    sample_rate: u32,
    frame_samples: usize,
    fillers: Vec<Filler>,
}

/// 16 kHz mono: the one format the lip-sync solver takes without resampling
/// (Source/VRPhysProto/VRSpeechFace.h:33).
pub const SAMPLE_RATE: u32 = 16000;
/// 20 ms at 16 kHz, equal to `FVRSpeechFace::ChunkSamples`, which is the frame length
/// Docs/DESIGN_Pet.md:424 says the page consumes.
pub const FRAME_SAMPLES: usize = 320;

const RAW: &str = include_str!("fillers.json");

/// The bank, parsed once. A malformed bank is a bug in the packer, not a run-time condition, so
/// this panics with the reason rather than degrading into a character that never makes a sound for
/// a cause nobody can see.
pub fn bank() -> &'static [Filler] {
    static BANK: OnceLock<Vec<Filler>> = OnceLock::new();
    BANK.get_or_init(|| parse_bank(RAW.as_bytes()).unwrap_or_else(|why| panic!("voice/fillers.json is not the bank: {why}")))
}

/// A bank file, the compiled one or a character's own `fillers.json`, with every filler checked
/// against the shape the page plays it by: the page reads `samples` samples out of `pcm_b64` and
/// indexes `envelope` by the 320-sample frame, so a count that disagrees with either is audio that
/// throws or a mouth that reads past its envelope. A character's file may be anyone's work, so it
/// is checked here once instead of failing on the page at every turn.
pub fn parse_bank(bytes: &[u8]) -> Result<Vec<Filler>, String> {
    let file: BankFile = serde_json::from_slice(bytes).map_err(|e| format!("it is not a filler bank: {e}"))?;
    if file.sample_rate != SAMPLE_RATE {
        return Err(format!("its sample_rate is {}, and the page plays {SAMPLE_RATE}", file.sample_rate));
    }
    if file.frame_samples != FRAME_SAMPLES {
        return Err(format!("its frame_samples is {}, and the mouth reads {FRAME_SAMPLES}", file.frame_samples));
    }
    if file.fillers.is_empty() {
        return Err("it holds no filler".to_string());
    }
    for f in &file.fillers {
        if f.samples == 0 || f.samples != f.envelope.len() * FRAME_SAMPLES {
            return Err(format!("{}: {} samples is not {} envelope frames of {FRAME_SAMPLES}", f.name, f.samples, f.envelope.len()));
        }
        if f.pcm_b64.len() != b64_len(f.samples * 2) {
            return Err(format!("{}: pcm_b64 is {} characters, and {} samples of PCM16 is {}", f.name, f.pcm_b64.len(), f.samples, b64_len(f.samples * 2)));
        }
        if !f.envelope.iter().all(|v| (0.0..=1.0).contains(v)) {
            return Err(format!("{}: an envelope value is outside 0..1", f.name));
        }
    }
    Ok(file.fillers)
}

/// Which thinking fillers the character on screen plays (Docs/DESIGN_Characters.md 5).
#[derive(Debug)]
pub enum Bank {
    /// The compiled bank, which is 小毛's voice.
    Default,
    /// Her own `fillers.json`.
    Own(Vec<Filler>),
    /// None at all: she has a voice of her own and no fillers in it, and a filler in 小毛's voice
    /// would be someone else thinking out loud.
    Quiet,
}

impl Bank {
    /// The rule, from what reading her `fillers.json` came to (`Ok(None)` when she has none) and
    /// whether she has a voice of her own. A file that is there and cannot be used counts as no
    /// file, and the reason comes back beside the bank for the caller to log, because a broken file
    /// that only changes which sound plays would otherwise never be noticed.
    pub fn of(own: Result<Option<Vec<u8>>, String>, has_voice: bool) -> (Bank, Option<String>) {
        let (own, why) = match own.and_then(|bytes| bytes.map(|b| parse_bank(&b)).transpose()) {
            Ok(own) => (own, None),
            Err(why) => (None, Some(why)),
        };
        let bank = match (own, has_voice) {
            (Some(fillers), _) => Bank::Own(fillers),
            (None, true) => Bank::Quiet,
            (None, false) => Bank::Default,
        };
        (bank, why)
    }

    pub fn fillers(&self) -> &[Filler] {
        match self {
            Bank::Default => bank(),
            Bank::Own(fillers) => fillers,
            Bank::Quiet => &[],
        }
    }
}

/// How the log says which bank she plays.
impl std::fmt::Display for Bank {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bank::Default => f.write_str("the default fillers"),
            Bank::Own(fillers) => write!(f, "her own {} fillers", fillers.len()),
            Bank::Quiet => f.write_str("no filler, because she has her own voice and no fillers in it"),
        }
    }
}

/// The bank of the character on screen and whose it is. One is held at a time and it is read once:
/// a file of up to 1 MiB parsed on every turn would be work repeated for an answer that cannot have
/// changed. Showing another character is what makes it read again.
#[derive(Default)]
pub struct Shown(Mutex<Option<(String, Bank)>>);

impl Shown {
    /// The filler for `turn` from the bank of the character `id`, which `load` makes only when the
    /// bank held is someone else's.
    pub fn pick(&self, id: &str, turn: usize, load: impl FnOnce() -> Bank) -> Option<Filler> {
        let mut held = self.0.lock().expect("fillers poisoned");
        let bank = match held.take() {
            Some((was, bank)) if was == id => bank,
            _ => load(),
        };
        let chosen = pick(bank.fillers(), turn).cloned();
        *held = Some((id.to_string(), bank));
        chosen
    }
}

/// Which filler plays on turn `turn`. Round-robin, and that is deliberate.
///
/// Round-robin guarantees no immediate repeat without a randomness source this crate does not
/// otherwise depend on, and it lets `pet_filler` assert the whole sequence. A filler bank is one
/// place where predictable costs nothing and a repeat is the thing that grates: six different
/// lines in rotation vary more than a random draw from six, which repeats one in six times.
///
/// Pure, so the rule is testable without a Tauri app. `None` only when the bank is empty, which
/// [`bank`] already refuses, so a caller holding a non-empty slice always gets a filler.
pub fn pick(fillers: &[Filler], turn: usize) -> Option<&Filler> {
    if fillers.is_empty() {
        return None;
    }
    Some(&fillers[turn % fillers.len()])
}

/// The base64 length of `bytes` bytes, which is how the bank's integrity is checked without a
/// decoder in this crate. Standard base64 with padding is four characters per three bytes, rounded
/// up.
pub const fn b64_len(bytes: usize) -> usize {
    (bytes + 2) / 3 * 4
}
