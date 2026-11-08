//! The one place audio changes rate (Docs/DESIGN_STT.md 1.1). Everything past `Boundary` is 16 kHz
//! mono i16, and `Frames16k` has no public constructor, so code outside this crate cannot make
//! 16 kHz audio any other way than by passing through here.

use std::f64::consts::PI;

/// `FVRSpeechFace::SampleRate`, and what SenseVoice and Silero both take.
pub const SAMPLE_RATE: u32 = 16_000;
/// 20 ms, `FVRSpeechFace::ChunkSamples`. The segmenter decides speech per frame of this length.
pub const FRAME: usize = 320;

/// 16 kHz, mono, i16.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frames16k(Vec<i16>);

impl Frames16k {
    pub(crate) fn from_vec(samples: Vec<i16>) -> Self {
        Frames16k(samples)
    }

    pub fn samples(&self) -> &[i16] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn ms(&self) -> u64 {
        self.0.len() as u64 * 1000 / u64::from(SAMPLE_RATE)
    }

    /// What the recogniser and the VAD take: the same samples scaled to -1..1.
    pub fn to_f32(&self) -> Vec<f32> {
        self.0.iter().map(|&s| f32::from(s) / 32768.0).collect()
    }
}

/// What the device hands over, before anything is done to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeFormat {
    pub rate: u32,
    pub channels: u16,
}

/// The device and what it gives, and whether this process converted it. Printed in `ready`, so
/// "converted once" is something a reader of the log can see and not a claim in a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFormat {
    pub device: String,
    pub native: NativeFormat,
    pub resampled: bool,
}

/// Interleaved samples as the device delivered them.
#[derive(Debug, Clone, Copy)]
pub enum Pcm<'a> {
    I16(&'a [i16]),
    F32(&'a [f32]),
}

impl Pcm<'_> {
    fn len(&self) -> usize {
        match self {
            Pcm::I16(s) => s.len(),
            Pcm::F32(s) => s.len(),
        }
    }

    fn at(&self, i: usize) -> f32 {
        match self {
            Pcm::I16(s) => f32::from(s[i]) / 32768.0,
            Pcm::F32(s) => s[i],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    NoChannels,
    Rate { rate: u32 },
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::NoChannels => write!(f, "the device reports zero channels"),
            FormatError::Rate { rate } => write!(f, "the device rate {rate} Hz is outside 8000..=384000"),
        }
    }
}

/// The capture boundary. One per opened stream, because the resampler carries history between
/// pushes and a second stream must not inherit the first one's tail.
pub struct Boundary {
    format: NativeFormat,
    resampler: Option<Sinc>,
    /// Samples of an interleaved frame that a push cut in half.
    partial: Vec<f32>,
    mono: Vec<f32>,
}

impl Boundary {
    pub fn new(format: NativeFormat) -> Result<Self, FormatError> {
        if format.channels == 0 {
            return Err(FormatError::NoChannels);
        }
        if !(8_000..=384_000).contains(&format.rate) {
            return Err(FormatError::Rate { rate: format.rate });
        }
        let resampler = (format.rate != SAMPLE_RATE).then(|| Sinc::new(format.rate, SAMPLE_RATE));
        Ok(Boundary { format, resampler, partial: Vec::new(), mono: Vec::new() })
    }

    pub fn resampled(&self) -> bool {
        self.resampler.is_some()
    }

    pub fn format(&self) -> NativeFormat {
        self.format
    }

    pub fn push(&mut self, pcm: Pcm) -> Frames16k {
        let channels = usize::from(self.format.channels);
        // 16 kHz mono i16 with nothing carried over is the case `resample_once` holds to bit
        // exactness, so it does not take the float round trip.
        if let (None, 1, Pcm::I16(s)) = (&self.resampler, channels, pcm) {
            return Frames16k(s.to_vec());
        }

        self.mono.clear();
        let mut i = 0;
        if !self.partial.is_empty() {
            while self.partial.len() < channels && i < pcm.len() {
                self.partial.push(pcm.at(i));
                i += 1;
            }
            if self.partial.len() < channels {
                return Frames16k::default();
            }
            self.mono.push(self.partial.iter().sum::<f32>() / channels as f32);
            self.partial.clear();
        }
        while i + channels <= pcm.len() {
            let sum: f32 = (i..i + channels).map(|k| pcm.at(k)).sum();
            self.mono.push(sum / channels as f32);
            i += channels;
        }
        self.partial.extend((i..pcm.len()).map(|k| pcm.at(k)));

        match &mut self.resampler {
            None => Frames16k(self.mono.iter().map(|&x| to_i16(x)).collect()),
            Some(r) => Frames16k(r.process(&self.mono).into_iter().map(to_i16).collect()),
        }
    }

    /// The output still held back by the filter's look-ahead. Called once, when the stream ends.
    pub fn finish(&mut self) -> Frames16k {
        match &mut self.resampler {
            None => Frames16k::default(),
            Some(r) => Frames16k(r.flush().into_iter().map(to_i16).collect()),
        }
    }
}

fn to_i16(x: f32) -> i16 {
    (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

/// Kaiser-windowed sinc at an arbitrary ratio, streaming.
///
/// The cut-off sits under the lower Nyquist with a transition that ends at it, so what folds back
/// on a downsample is what the window's stopband leaves, about 90 dB down. `resample_once`
/// measures that against a tone placed where it would fold.
struct Sinc {
    /// Input samples per output sample.
    step: f64,
    /// Half the kernel length, in input samples.
    half: usize,
    /// `PHASES + 1` rows of `2 * half` taps; a row is the kernel at one sub-sample offset.
    table: Vec<f32>,
    /// Input not yet fully used, starting `half` samples before the next output's centre.
    history: Vec<f32>,
    /// Where the next output's centre is, in input samples, counted from `history[0]`.
    at: f64,
}

const PHASES: usize = 256;
const KAISER_BETA: f64 = 9.0;
/// Kernel zero crossings each side, at the lower of the two rates.
const ZERO_CROSSINGS: f64 = 64.0;

impl Sinc {
    fn new(from: u32, to: u32) -> Self {
        let step = f64::from(from) / f64::from(to);
        let lower = f64::from(from.min(to));
        // Transition from 0.95 of the lower Nyquist to the Nyquist itself.
        let cutoff = 0.5 * lower * 0.975 / f64::from(from);
        let half = (ZERO_CROSSINGS * step.max(1.0)).ceil() as usize;
        let taps = 2 * half;
        let mut table = vec![0f32; (PHASES + 1) * taps];
        let i0_beta = bessel_i0(KAISER_BETA);
        for phase in 0..=PHASES {
            let frac = phase as f64 / PHASES as f64;
            for k in 0..taps {
                // Tap k is input sample `centre_floor - half + 1 + k`; its distance from the centre.
                let d = (k as f64 - (half as f64 - 1.0)) - frac;
                let x = d / half as f64;
                let w = if x.abs() >= 1.0 { 0.0 } else { bessel_i0(KAISER_BETA * (1.0 - x * x).sqrt()) / i0_beta };
                let arg = 2.0 * cutoff * d;
                let sinc = if arg.abs() < 1e-12 { 1.0 } else { (PI * arg).sin() / (PI * arg) };
                table[phase * taps + k] = (2.0 * cutoff * sinc * w) as f32;
            }
        }
        Sinc { step, half, table, history: vec![0.0; half], at: half as f64 }
    }

    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.history.extend_from_slice(input);
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 2);
        let taps = 2 * self.half;
        loop {
            let floor = self.at.floor() as usize;
            if floor + self.half >= self.history.len() {
                break;
            }
            let pos = (self.at - floor as f64) * PHASES as f64;
            let p = (pos.floor() as usize).min(PHASES - 1);
            let mix = (pos - p as f64) as f32;
            let lo = &self.table[p * taps..(p + 1) * taps];
            let hi = &self.table[(p + 1) * taps..(p + 2) * taps];
            let window = &self.history[floor + 1 - self.half..floor + 1 + self.half];
            let mut acc = 0f32;
            for k in 0..taps {
                acc += window[k] * (lo[k] + (hi[k] - lo[k]) * mix);
            }
            out.push(acc);
            self.at += self.step;
        }
        let keep_from = (self.at.floor() as usize + 1).saturating_sub(self.half);
        self.history.drain(..keep_from);
        self.at -= keep_from as f64;
        out
    }

    fn flush(&mut self) -> Vec<f32> {
        // Only outputs whose centre falls on real input; the rest would be the zero padding's.
        let keep = ((self.history.len() as f64 - self.at) / self.step).ceil().max(0.0) as usize;
        let pad = vec![0.0; self.half + 1 + self.step.ceil() as usize];
        let mut out = self.process(&pad);
        out.truncate(keep.min(out.len()));
        out
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}
