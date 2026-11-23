// The character's voice. The spoken text of a turn goes to a speech service and comes back as a
// WAV, which this file turns into 16 kHz mono PCM16 and rides on the `utterance` event with its
// loudness envelope.
//
// A turn with no PCM is a different result, not an empty one (Docs/DESIGN_Voice.md 3.2). `Voice`
// is that split, and a `Speech` cannot hold zero samples, so the page never gets a voiced turn it
// has nothing to play for. Nothing here invents an envelope: no PCM, no mouth (3.2, the last
// paragraph).
//
// Which service is the user's to say (Docs/DESIGN_Settings.md 4), through four variables the core
// reads once, when it starts. `Service::read` turns them into the one thing the rest of this file
// works from, or into the reason there is no voice, and a reason is spoken by every turn instead
// of stopping the core.

use crate::character::VoiceId;
use base64::prelude::{BASE64_STANDARD, Engine as _};
use serde::Serialize;
use serde_json::{Value, json};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::time::Duration;

pub const SAMPLE_RATE: u32 = 16_000;
/// 20 ms at 16 kHz, `FVRSpeechFace::ChunkSamples`, the frame the page's mouth reads
/// (Docs/DESIGN_Pet.md 4).
pub const FRAME_SAMPLES: usize = 320;
/// `FVRSpeechFace::SilenceRms`, about -50 dBFS (Source/VRPhysProto/VRSpeechFace.h:40).
const SILENCE_RMS: f64 = 0.00316;

/// Points the core at another speech service: `https://` and any host, or plain `http://` on a
/// loopback literal. Plain http to another machine would carry the character's words across the
/// network in the clear, so it is refused (Docs/DESIGN_Voice.md 8, the answer on LAN plain HTTP).
pub const URL_ENV: &str = "VRCHATCORE_SPEECH_URL";
/// The `model` field the service is sent. Unset is `MODEL`.
pub const MODEL_ENV: &str = "VRCHATCORE_SPEECH_MODEL";
/// The `voice` field the service is sent, for every character. Unset means the character's own
/// voice id, which only the local service knows how to look up (a `voice.wav` in her folder).
pub const VOICE_ENV: &str = "VRCHATCORE_SPEECH_VOICE";
/// The NAME of the variable that holds the API key, never the key. The key is sent as
/// `Authorization: Bearer` and goes nowhere else.
pub const KEY_ENV_ENV: &str = "VRCHATCORE_SPEECH_KEY_ENV";
const DEFAULT_BASE: &str = "http://127.0.0.1:8880/v1";
/// What `Scripts/tts_engines/serve.py` serves. It reads neither, and a service that does may
/// refuse names it does not know, which then reaches the bubble as its own words.
const MODEL: &str = "voxcpm2";
/// Longest `model`, `voice` or key variable name taken from the environment. They are names and
/// not text, and a name that long is a paste of something else.
const NAME_MAX: usize = 64;
const KEY_ENV_NAME_MAX: usize = 40;

/// A loopback service either accepts at once or is not there.
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// A service across the network needs a DNS answer, a TCP handshake and a TLS handshake first.
const REMOTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Not measured yet (Docs/DESIGN_Voice.md 8, item 3). The service answers with the whole clip, so
/// the wait is the reply's length times the engine's RTF: 0.62 to 0.65 measured on this machine
/// (2026-11-05, three sentences twice each), so a long three-clause reply of about 20 s of speech
/// takes about 13 s. 30 s covers that with room for a GPU that another app is using, and is short
/// enough that a hung service costs one turn its voice and not the conversation.
const SPEECH_TIMEOUT: Duration = Duration::from_secs(30);

/// How a turn sounds. On the wire it is the `voice` field of `utterance`, tagged by `state`.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Voice {
    Voiced(Speech),
    /// `why` is one sentence the bubble can show as it is.
    Silent { why: String },
}

/// At least one sample of 16 kHz mono PCM16. The shape of one entry of the pet's `fillers.json`
/// (`samples`, `pcm_b64`, `envelope`) on purpose, so real speech and the fillers share one decode
/// path on the page.
#[derive(Debug, PartialEq, Serialize)]
pub struct Speech {
    samples: usize,
    /// Little-endian PCM16, standard base64 with padding.
    pcm_b64: String,
    envelope: Vec<f32>,
}

impl Speech {
    /// `None` for no samples, which is a silent turn and not a voiced one.
    pub fn from_pcm(pcm: &[i16]) -> Option<Speech> {
        if pcm.is_empty() {
            return None;
        }
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        Some(Speech { samples: pcm.len(), pcm_b64: BASE64_STANDARD.encode(bytes), envelope: envelope(pcm) })
    }

    /// How long it plays.
    pub fn seconds(&self) -> f64 {
        self.samples as f64 / f64::from(SAMPLE_RATE)
    }
}

/// One loudness value per `FRAME_SAMPLES`, as `envelope()` in Scripts/tts_engines/fillers.py
/// computes it for the fillers: RMS per whole frame (a partial last frame has none), frames under
/// the solver's silence floor are 0, the rest are divided by the loudest frame of this clip and
/// rounded to four places. No smoothing; the page interpolates and the face shapes it.
pub fn envelope(pcm: &[i16]) -> Vec<f32> {
    let rms: Vec<f64> = pcm
        .chunks_exact(FRAME_SAMPLES)
        .map(|frame| {
            let power = frame.iter().map(|&s| (f64::from(s) / 32768.0).powi(2)).sum::<f64>() / FRAME_SAMPLES as f64;
            (power + 1e-20).sqrt()
        })
        .collect();
    let peak = rms.iter().copied().fold(0.0, f64::max);
    if peak <= 0.0 {
        return vec![0.0; rms.len()];
    }
    rms.iter()
        .map(|&r| if r < SILENCE_RMS { 0.0 } else { ((r / peak) * 10_000.0).round() as f32 / 10_000.0 })
        .collect()
}

/// What a WAV's `fmt ` chunk says that matters here.
struct Format {
    channels: usize,
    rate: u32,
}

/// Sample rates a clip may have. Below the first is not speech a person would choose, and the
/// resampler's table grows as the rate falls away from 16 kHz, so the top needs a bound too.
const RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;
const MAX_CHANNELS: usize = 8;

/// The samples of a RIFF/WAVE file that is PCM16, as 16 kHz mono, or what is wrong with it in
/// words a person can read.
///
/// A clip that is 16 kHz mono comes through untouched. Another rate is resampled and more than one
/// channel is averaged, because a speech service of the user's choosing is free to answer at its
/// own rate (OpenAI's `wav` is 24 kHz). Anything that is not 16-bit integer PCM is refused: float,
/// 8-bit, and anything compressed would each need a decoder of their own.
pub fn pcm_of_wav(bytes: &[u8]) -> Result<Vec<i16>, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("它不是 WAV 檔（開頭是 {:?}）", String::from_utf8_lossy(&bytes[..bytes.len().min(12)])));
    }
    let mut format = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let mut size = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]) as usize;
        let body = &bytes[at + 8..];
        // A WAV written while it streams cannot know its length yet, and says so with all ones.
        // It runs to the end of the file, and a last half-frame is where the stream was cut.
        let open_ended = id == b"data" && size == u32::MAX as usize;
        if open_ended {
            size = body.len();
        }
        if size > body.len() {
            return Err(format!("{:?} 區塊說它有 {size} 位元組，檔案只剩 {}", String::from_utf8_lossy(id), body.len()));
        }
        let body = &body[..size];
        match (id, &format) {
            (b"fmt ", _) => format = Some(check_format(body)?),
            (b"data", None) => return Err("聲音資料出現在格式說明之前".to_string()),
            (b"data", Some(format)) => {
                let frame = 2 * format.channels;
                let body = if open_ended { &body[..size - size % frame] } else { body };
                if !body.len().is_multiple_of(2) {
                    return Err(format!("聲音資料有 {} 位元組，不是 16 位元取樣的整數倍", body.len()));
                }
                if !body.len().is_multiple_of(frame) {
                    return Err(format!("聲音資料有 {} 位元組，不是 {} 個聲道的整數倍", body.len(), format.channels));
                }
                let samples: Vec<i16> = body.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                return Ok(mono_16k(samples, format));
            }
            _ => {}
        }
        at += 8 + size + size % 2;
    }
    Err("檔案裡沒有聲音資料".to_string())
}

fn check_format(fmt: &[u8]) -> Result<Format, String> {
    if fmt.len() < 16 {
        return Err(format!("格式說明只有 {} 位元組", fmt.len()));
    }
    let u16_at = |i: usize| u16::from_le_bytes([fmt[i], fmt[i + 1]]);
    let (tag, channels, rate, bits) = (u16_at(0), usize::from(u16_at(2)), u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]), u16_at(14));
    if tag != 1 {
        return Err(format!("編碼是 {tag}，不是 PCM（1）"));
    }
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(format!("它有 {channels} 個聲道，這裡收 1 到 {MAX_CHANNELS} 個"));
    }
    if !RATES.contains(&rate) {
        return Err(format!("取樣率是 {rate} Hz，這裡收 {} 到 {} Hz", RATES.start(), RATES.end()));
    }
    if bits != 16 {
        return Err(format!("每個取樣 {bits} 位元，不是 16 位元"));
    }
    Ok(Format { channels, rate })
}

/// Interleaved `samples` as one channel at `SAMPLE_RATE`.
fn mono_16k(samples: Vec<i16>, format: &Format) -> Vec<i16> {
    let mono = if format.channels == 1 {
        samples
    } else {
        let n = format.channels as i32;
        samples.chunks_exact(format.channels).map(|frame| (frame.iter().map(|&s| i32::from(s)).sum::<i32>() / n) as i16).collect()
    };
    if format.rate == SAMPLE_RATE { mono } else { resample(&mono, format.rate) }
}

/// Zero crossings of the sinc on each side of an output sample, at the lower of the two rates.
/// More is a steeper filter and a longer table. At 16, measured on 24 kHz clips: 7 kHz comes
/// through 0.7 dB down, 8.5 kHz is 38 dB down and everything from 9 kHz up is below 80 dB, in a
/// table of two phases of 52 taps. Twenty seconds of speech converts in about 0.3 s in a debug
/// build.
const ZEROS: usize = 16;
/// Where the filter turns off, as a share of the lower rate's Nyquist. The window needs room
/// between the passband and what must go, and speech has little above 7.6 kHz.
const PASSBAND: f64 = 0.95;

/// `input` at `rate` as 16 kHz, by a Blackman-windowed sinc. The low-pass is the point: stepping
/// from 24 kHz to 16 kHz by interpolation alone folds everything between 8 and 12 kHz back below 8
/// as a hiss that was never in the voice (the same check Docs/DESIGN_STT.md 1.1 holds the
/// microphone's resampler to).
///
/// The ratio is a fraction in lowest terms, so only `phases` different offsets between an output
/// sample and the input ever occur, and each one's weights are made once.
fn resample(input: &[i16], rate: u32) -> Vec<i16> {
    let (mut a, mut b) = (rate, SAMPLE_RATE);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let step = (rate / a) as usize;
    let phases = (SAMPLE_RATE / a) as usize;
    let cutoff = PASSBAND * f64::min(1.0, f64::from(SAMPLE_RATE) / f64::from(rate));
    let half = (ZEROS as f64 / cutoff).ceil() as i64;
    let weights: Vec<Vec<f64>> = (0..phases)
        .map(|phase| {
            let offset = phase as f64 / phases as f64;
            let mut row: Vec<f64> = (0..2 * half)
                .map(|tap| {
                    let x = (tap - half + 1) as f64 - offset;
                    let u = x / half as f64;
                    let window = 0.42 + 0.5 * (std::f64::consts::PI * u).cos() + 0.08 * (2.0 * std::f64::consts::PI * u).cos();
                    let arg = std::f64::consts::PI * cutoff * x;
                    let sinc = if arg == 0.0 { 1.0 } else { arg.sin() / arg };
                    cutoff * sinc * window
                })
                .collect();
            // Every phase passes a steady level at the level it came in.
            let sum: f64 = row.iter().sum();
            row.iter_mut().for_each(|w| *w /= sum);
            row
        })
        .collect();
    (0..input.len() * phases / step)
        .map(|n| {
            let (whole, phase) = (n * step / phases, n * step % phases);
            let first = whole as i64 - half + 1;
            let sum: f64 = weights[phase]
                .iter()
                .enumerate()
                .map(|(tap, w)| usize::try_from(first + tap as i64).ok().and_then(|i| input.get(i)).map_or(0.0, |&s| w * f64::from(s)))
                .sum();
            sum.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
        })
        .collect()
}

/// Why a turn has no voice. Each one becomes a sentence a person can act on. None of them holds
/// the API key, or anything that could be one: what a person typed into a URL, a model or a key
/// variable's name is shown only where it cannot be a secret.
#[derive(Clone, Debug, PartialEq)]
pub enum Fault {
    /// `URL_ENV` is not https and not plain http on a loopback literal. `value` is `shown`.
    BadUrl { value: String },
    /// `URL_ENV` is https and this build has no TLS.
    NoTls { url: String },
    /// `MODEL_ENV` or `VOICE_ENV` is not a name.
    BadName { env: &'static str },
    /// A service across the network cannot look up a character's folder, so it has to be told the
    /// voice.
    NeedsVoice { url: String },
    /// `KEY_ENV_ENV` is not a variable name of the shape `valid_key_env` accepts, which is how a
    /// key pasted there is told from a name.
    KeyEnvName,
    /// The variable `KEY_ENV_ENV` names is not set, or is empty.
    KeyMissing { name: String },
    /// The variable holds something that cannot ride in a header.
    KeyUnusable { name: String },
    /// Nothing answered at the address of the local service: refused, or no handshake within
    /// `LOCAL_CONNECT_TIMEOUT`.
    NotRunning { url: String },
    /// Nothing answered at the address of a service elsewhere.
    Unreachable { url: String },
    TimedOut,
    Status { code: u16, said: String },
    Broken(String),
    Incompatible(String),
    Empty,
    NothingToSay,
}

impl Fault {
    pub fn why(&self) -> String {
        match self {
            Fault::BadUrl { value } => format!(
                "環境變數 {URL_ENV} 是 {value:?}，這裡只接受 https:// 開頭的位址，或 http://127.0.0.1、http://[::1] 這種本機位址，所以這句沒有聲音。用 http 連別台機器，她說的每一句話都會在網路上被看光。改掉它，或刪掉它改用預設的 {DEFAULT_BASE}。"
            ),
            Fault::NoTls { url } => format!(
                "{URL_ENV} 指到 {url}，但這個核心編譯時沒有帶 TLS（cargo build --features tls），連不了 https，所以這句沒有聲音。換一個帶 TLS 的核心，或改用 http://127.0.0.1 的本機服務。"
            ),
            Fault::BadName { env } => format!(
                "環境變數 {env} 不是一個名稱：它是 1 到 {NAME_MAX} 個字元，只能有英文字母、數字和 . _ - : /，所以這句沒有聲音。改掉它或刪掉它。"
            ),
            Fault::NeedsVoice { url } => format!(
                "語音服務 {url} 不在這台機器上，它不認得角色的聲音代號，所以要用 {VOICE_ENV} 指定它的聲音（例如 OpenAI 的 alloy），這句沒有聲音。"
            ),
            Fault::KeyEnvName => format!(
                "環境變數 {KEY_ENV_ENV} 要填的是放金鑰的環境變數的名稱，不是金鑰本身：名稱只能有大寫英文字母、數字和底線，不超過 {KEY_ENV_NAME_MAX} 個字元，所以這句沒有聲音。"
            ),
            Fault::KeyMissing { name } => format!(
                "金鑰放在環境變數 {name}（{KEY_ENV_ENV} 指到它），但它沒有設值，所以每一句都沒有聲音。把金鑰放進 {name} 再重開桌寵，或不要設 {KEY_ENV_ENV}。"
            ),
            Fault::KeyUnusable { name } => {
                format!("環境變數 {name} 裡的金鑰不能放進請求標頭：它只能有可見的英文字母、數字和符號，不能有空白或換行，所以每一句都沒有聲音。")
            }
            Fault::NotRunning { url } => format!(
                "本機語音服務沒有開（{url} 連不上），所以這句只有字。在 Scripts/tts_engines 用裝了 VoxCPM2 的 Python 跑 python serve.py，等它印出 ready 就有聲音了。"
            ),
            Fault::Unreachable { url } => format!("語音服務 {url} 連不上（網路、網址或憑證有問題），所以這句只有字。"),
            Fault::TimedOut => format!("語音服務 {} 秒內沒有念完這句，所以這句只有字。", SPEECH_TIMEOUT.as_secs()),
            Fault::Status { code, said } if matches!(code, 401 | 403) => format!(
                "語音服務回了 HTTP {code}（{said}），它不接受這把金鑰，所以這句只有字。檢查 {KEY_ENV_ENV} 指到的那個環境變數。"
            ),
            Fault::Status { code, said } => format!("語音服務回了 HTTP {code}（{said}），所以這句只有字。"),
            Fault::Broken(what) => format!("跟語音服務的連線出了問題（{what}），所以這句只有字。"),
            Fault::Incompatible(what) => {
                format!("語音服務給的聲音格式我讀不懂：{what}。這裡只收 16 位元整數的 PCM WAV，取樣率與聲道數不拘。")
            }
            Fault::Empty => "語音服務回了一段沒有任何取樣的聲音，所以這句只有字。".to_string(),
            Fault::NothingToSay => "這句沒有要念出來的字。".to_string(),
        }
    }
}

/// `value` as a refusal may print it: without the user name and password of the address, and
/// without a query or fragment, which are the two other places a key gets put by hand.
pub(crate) fn shown(value: &str) -> String {
    let value = &value[..value.find(['?', '#']).unwrap_or(value.len())];
    // With no scheme it is not an address, and whatever it is stays unsaid.
    let Some((scheme, rest)) = value.split_once("://") else { return "不是網址".to_string() };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    format!("{scheme}://{}{path}", authority.rsplit_once('@').map_or(authority, |(_, host)| host))
}

/// Where requests go, and what kind of place that is.
#[derive(Clone, Debug, PartialEq)]
pub struct Endpoint {
    /// `{base}/audio/speech`.
    pub url: String,
    pub tls: bool,
    /// The loopback literal `127.0.0.1` or `[::1]`: this machine, which waits for nobody.
    pub local: bool,
}

/// The speech endpoint. `None` or an empty value is the default base.
pub fn speech_url(base: Option<OsString>) -> Result<Endpoint, Fault> {
    let Some(raw) = base.filter(|v| !v.is_empty()) else {
        return Ok(Endpoint { url: format!("{DEFAULT_BASE}/audio/speech"), tls: false, local: true });
    };
    let value = raw.to_string_lossy().into_owned();
    endpoint(&value, "/audio/speech").ok_or_else(|| Fault::BadUrl { value: shown(&value) })
}

/// `base` with `path` after it, when `base` is `https://` and any host, or plain `http://` on a
/// loopback literal. The rule for every service the core sends words to, speech and chat alike.
pub(crate) fn endpoint(base: &str, path: &str) -> Option<Endpoint> {
    let (tls, rest) = match (base.strip_prefix("https://"), base.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (_, Some(rest)) => (false, rest),
        _ => return None,
    };
    let (authority, base_path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = host_of(authority)?;
    let local = matches!(host, "127.0.0.1" | "[::1]");
    if (!tls && !local)
        || !base_path.is_ascii()
        || base_path.contains(['?', '#'])
        || base_path.contains(|c: char| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let scheme = if tls { "https" } else { "http" };
    Some(Endpoint { url: format!("{scheme}://{authority}{}{path}", base_path.trim_end_matches('/')), tls, local })
}

/// The host of `authority` (`host`, `host:port`, `[v6]` or `[v6]:port`) when all of it is well
/// formed: a port is 1 to 65535, a name is dot-separated labels of letters, digits and hyphens, and
/// there is no user name, which is how `http://127.0.0.1@evil.example` gets read as loopback.
fn host_of(authority: &str) -> Option<&str> {
    let (host, port) = if authority.starts_with('[') {
        authority.split_at(authority.find(']')? + 1)
    } else {
        authority.split_at(authority.find(':').unwrap_or(authority.len()))
    };
    let port_ok = port.is_empty() || port.strip_prefix(':').is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n > 0));
    let host_ok = match host.strip_prefix('[') {
        Some(v6) => v6.strip_suffix(']').is_some_and(|a| !a.is_empty() && a.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')),
        None => {
            host.len() <= 253
                && host.split('.').all(|label| {
                    (1..=63).contains(&label.len())
                        && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                })
        }
    };
    (port_ok && host_ok).then_some(host)
}

/// What the four variables say, as they were in the environment.
#[derive(Default)]
pub struct SpeechEnv {
    pub url: Option<OsString>,
    pub model: Option<OsString>,
    pub voice: Option<OsString>,
    pub key_env: Option<OsString>,
}

impl SpeechEnv {
    pub fn from_process() -> SpeechEnv {
        SpeechEnv {
            url: std::env::var_os(URL_ENV),
            model: std::env::var_os(MODEL_ENV),
            voice: std::env::var_os(VOICE_ENV),
            key_env: std::env::var_os(KEY_ENV_ENV),
        }
    }
}

/// The API key. `Debug` is written out so that nothing that prints a `Service`, in a test failure
/// or a later log line, prints this.
pub(crate) struct Key(String);

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}

impl Key {
    /// The `Authorization` header's value, the one place the key is sent.
    pub(crate) fn bearer(&self) -> String {
        format!("Bearer {}", self.0)
    }

    /// `text` with the key in it replaced, for a service that explains a refusal by quoting the key
    /// it was given. Before any cut, so a cut cannot leave half of it.
    pub(crate) fn redact(&self, text: &str) -> String {
        text.replace(&self.0, "***")
    }
}

/// Why the variable a key is read from cannot give one.
#[derive(Debug, PartialEq)]
pub(crate) enum KeyFault {
    /// Not a name of the shape `valid_key_env` accepts.
    Name,
    Missing(String),
    Unusable(String),
}

/// The key in the variable `name` names, read through `lookup`.
pub(crate) fn key_from(name: &OsStr, lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Key, KeyFault> {
    let name = name.to_str().filter(|n| valid_key_env(n)).ok_or(KeyFault::Name)?;
    let value = lookup(name).ok_or_else(|| KeyFault::Missing(name.to_string()))?;
    let value = value.to_str().ok_or_else(|| KeyFault::Unusable(name.to_string()))?.trim();
    if value.is_empty() {
        return Err(KeyFault::Missing(name.to_string()));
    }
    if !value.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(KeyFault::Unusable(name.to_string()));
    }
    Ok(Key(value.to_string()))
}

/// A name for a variable that holds a key: the shape environment variables conventionally have.
/// Narrower than the system allows on purpose. A key is long and has lower-case letters, so a key
/// pasted where the name goes is refused here, instead of being echoed back by "that variable is
/// not set".
fn valid_key_env(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= KEY_ENV_NAME_MAX
        && bytes.next().is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// A model or voice name a service is sent as given: 1 to `NAME_MAX` letters, digits and `. _ - : /`.
pub(crate) fn is_name(value: &str) -> bool {
    (1..=NAME_MAX).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_:/".contains(&b))
}

/// A `model` or `voice` sent as given: a name and not text.
fn name_of(env: &'static str, value: Option<&OsString>) -> Result<Option<String>, Fault> {
    let Some(value) = value.filter(|v| !v.is_empty()) else { return Ok(None) };
    value.to_str().filter(|v| is_name(v)).map(|v| Some(v.to_string())).ok_or(Fault::BadName { env })
}

/// Everything a request needs from the environment, checked.
#[derive(Debug)]
pub struct Service {
    endpoint: Endpoint,
    model: String,
    /// What is sent as `voice` whoever is speaking. `None` is the character's own.
    voice: Option<String>,
    key: Option<Key>,
}

/// One request, before it is sent. No `Debug`: `authorization` is the key.
pub struct Request {
    pub url: String,
    pub body: Value,
    pub authorization: Option<String>,
}

impl Service {
    /// `lookup` reads the variable that `env.key_env` names. The first thing wrong is the fault.
    pub fn read(env: &SpeechEnv, lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Service, Fault> {
        let endpoint = speech_url(env.url.clone())?;
        if endpoint.tls && !cfg!(feature = "tls") {
            return Err(Fault::NoTls { url: endpoint.url });
        }
        let model = name_of(MODEL_ENV, env.model.as_ref())?.unwrap_or_else(|| MODEL.to_string());
        let voice = name_of(VOICE_ENV, env.voice.as_ref())?;
        if !endpoint.local && voice.is_none() {
            return Err(Fault::NeedsVoice { url: endpoint.url });
        }
        let key = match env.key_env.as_ref().filter(|v| !v.is_empty()) {
            None => None,
            Some(name) => Some(key_from(name, lookup).map_err(|fault| match fault {
                KeyFault::Name => Fault::KeyEnvName,
                KeyFault::Missing(name) => Fault::KeyMissing { name },
                KeyFault::Unusable(name) => Fault::KeyUnusable { name },
            })?),
        };
        Ok(Service { endpoint, model, voice, key })
    }

    /// `text` spoken for a character whose voice id is `voice`. A voice the user named wins over
    /// the character's, whoever she is.
    pub fn request(&self, text: &str, voice: &VoiceId) -> Request {
        let voice = self.voice.as_deref().unwrap_or(voice.as_str());
        Request {
            url: self.endpoint.url.clone(),
            body: json!({ "model": self.model, "input": text, "voice": voice, "response_format": "wav" }),
            authorization: self.key.as_ref().map(Key::bearer),
        }
    }
}

/// The client for one speech service, made once when the core starts.
pub struct Speaker {
    service: Result<Service, Fault>,
    key_env: Option<OsString>,
    agent: ureq::Agent,
}

impl Speaker {
    pub fn new(env: SpeechEnv, lookup: &dyn Fn(&str) -> Option<OsString>) -> Speaker {
        let service = Service::read(&env, lookup);
        let remote = service.as_ref().is_ok_and(|s| !s.endpoint.local);
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(if remote { REMOTE_CONNECT_TIMEOUT } else { LOCAL_CONNECT_TIMEOUT }))
            .timeout_global(Some(SPEECH_TIMEOUT))
            .http_status_as_error(false)
            // A redirect would carry the key to a host the user did not name.
            .max_redirects(0)
            // Loopback never goes through a proxy (Docs/DESIGN_Voice.md 3.3), and ureq reads one
            // from the environment unless told not to. A service elsewhere is connected to
            // directly as well, so a machine that can only reach the internet through a proxy
            // gets `Unreachable`.
            .proxy(None)
            .build()
            .new_agent();
        Speaker { service, key_env: env.key_env.filter(|v| !v.is_empty()), agent }
    }

    /// The variable that holds the key, which `claude` is started without: it has no use for it.
    pub fn key_env(&self) -> Option<&OsStr> {
        self.key_env.as_deref()
    }

    /// The voice for `text`, which is the spoken text: no gesture markers, spoken as `voice`. The
    /// engine logs it, with the part it belongs to.
    pub fn voice(&self, text: &str, voice: &VoiceId) -> Voice {
        match self.speak(text, voice) {
            Ok(speech) => Voice::Voiced(speech),
            Err(fault) => Voice::Silent { why: fault.why() },
        }
    }

    fn speak(&self, text: &str, voice: &VoiceId) -> Result<Speech, Fault> {
        let service = self.service.as_ref().map_err(Clone::clone)?;
        if text.trim().is_empty() {
            return Err(Fault::NothingToSay);
        }
        let request = service.request(text, voice);
        let url = &request.url;
        let fault = |e: ureq::Error| match e {
            ureq::Error::Timeout(ureq::Timeout::Connect) | ureq::Error::ConnectionFailed => no_answer(service, url),
            ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => no_answer(service, url),
            ureq::Error::Timeout(_) => Fault::TimedOut,
            other => Fault::Broken(other.to_string()),
        };
        let mut post = self.agent.post(url).header("Content-Type", "application/json");
        if let Some(authorization) = &request.authorization {
            post = post.header("Authorization", authorization);
        }
        let mut response = post.send(request.body.to_string()).map_err(fault)?;
        let code = response.status().as_u16();
        let bytes = response.body_mut().read_to_vec().map_err(fault)?;
        if code != 200 {
            let said = String::from_utf8_lossy(&bytes).into_owned();
            let said = service.key.as_ref().map_or(said.clone(), |key| key.redact(&said));
            return Err(Fault::Status { code, said: said.chars().take(120).collect() });
        }
        let pcm = pcm_of_wav(&bytes).map_err(Fault::Incompatible)?;
        Speech::from_pcm(&pcm).ok_or(Fault::Empty)
    }
}

fn no_answer(service: &Service, url: &str) -> Fault {
    let url = url.to_string();
    if service.endpoint.local { Fault::NotRunning { url } } else { Fault::Unreachable { url } }
}
