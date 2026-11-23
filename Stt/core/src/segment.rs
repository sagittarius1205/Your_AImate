//! Speech inside one press (Docs/DESIGN_STT.md 2.3). Release ends the utterance; this only trims the
//! silence around speech, cuts out long pauses, bounds a segment's length, and says whether a press
//! held any speech at all. The VAD is a trait so every threshold here is tested to the frame with a
//! scripted mask and no audio.

use std::collections::VecDeque;

use crate::audio::{FRAME, Frames16k, SAMPLE_RATE};

/// Speech or not, one 20 ms frame at a time. Two implementations: Silero in the host, and a mask in
/// the self-tests.
pub trait Vad {
    fn voiced(&mut self, frame: &[i16]) -> bool;
    fn reset(&mut self);
}

/// Every value is a starting point to be measured on real speech (Docs/DESIGN_STT.md 2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmenterConfig {
    /// How far before its first voiced frame a segment starts. This is the room the detector's onset
    /// lag has to fit in: Silero called a soft first syllable voiced up to 1200 ms late, and audio it
    /// had not yet called voiced is dropped from the segment (Docs/DESIGN_STT.md 7.7).
    pub pad_pre_ms: u32,
    pub pad_post_ms: u32,
    pub split_silence_ms: u32,
    pub min_speech_ms: u32,
    pub max_segment_ms: u32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        SegmenterConfig { pad_pre_ms: 1500, pad_post_ms: 200, split_silence_ms: 700, min_speech_ms: 150, max_segment_ms: 30_000 }
    }
}

/// What a whole press held, which is what decides the event when there is no text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// Not one voiced frame.
    Nothing,
    /// Voiced frames, but no run of them reached `min_speech_ms`.
    TooShort,
    Speech,
}

const FRAME_MS: u32 = (FRAME as u32 * 1000) / SAMPLE_RATE;

fn frames_in(ms: u32) -> usize {
    ms.div_ceil(FRAME_MS) as usize
}

struct Building {
    samples: Vec<i16>,
    voiced_frames: usize,
    /// End of the last voiced frame, in samples into `samples`.
    voiced_end: usize,
    silent_run: usize,
}

pub struct Segmenter {
    cfg: SegmenterConfig,
    /// Samples short of a whole frame, kept for the next push.
    carry: Vec<i16>,
    /// The last `pad_pre_ms` of silence before speech starts.
    pre: VecDeque<i16>,
    building: Option<Building>,
    heard: Heard,
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Self {
        Segmenter { cfg, carry: Vec::new(), pre: VecDeque::new(), building: None, heard: Heard::Nothing }
    }

    /// Feeds audio and returns every segment it completed.
    pub fn push(&mut self, audio: &Frames16k, vad: &mut dyn Vad) -> Vec<Frames16k> {
        self.carry.extend_from_slice(audio.samples());
        let whole = self.carry.len() / FRAME * FRAME;
        let frames: Vec<i16> = self.carry.drain(..whole).collect();
        let mut done = Vec::new();
        for frame in frames.chunks_exact(FRAME) {
            let voiced = vad.voiced(frame);
            if let Some(seg) = self.frame(frame, voiced) {
                done.push(seg);
            }
        }
        done
    }

    /// The release. Returns the last segment, if it held enough speech, and what the press held.
    pub fn close(mut self) -> (Option<Frames16k>, Heard) {
        let last = self.building.take().and_then(|b| self.finish(b));
        (last, self.heard)
    }

    fn frame(&mut self, frame: &[i16], voiced: bool) -> Option<Frames16k> {
        if voiced && self.heard == Heard::Nothing {
            self.heard = Heard::TooShort;
        }
        let Some(b) = self.building.as_mut() else {
            if voiced {
                let mut samples: Vec<i16> = self.pre.drain(..).collect();
                samples.extend_from_slice(frame);
                let voiced_end = samples.len();
                self.building = Some(Building { samples, voiced_frames: 1, voiced_end, silent_run: 0 });
            } else {
                self.pre.extend(frame.iter().copied());
                let keep = frames_in(self.cfg.pad_pre_ms) * FRAME;
                while self.pre.len() > keep {
                    self.pre.pop_front();
                }
            }
            return None;
        };

        b.samples.extend_from_slice(frame);
        if voiced {
            b.voiced_frames += 1;
            b.voiced_end = b.samples.len();
            b.silent_run = 0;
        } else {
            b.silent_run += 1;
        }

        if b.silent_run >= frames_in(self.cfg.split_silence_ms) {
            let b = self.building.take()?;
            // The finished segment ends `pad_post_ms` into this silence, so the next one starts after it.
            let tail_from = (b.voiced_end + frames_in(self.cfg.pad_post_ms) * FRAME).min(b.samples.len());
            let keep = frames_in(self.cfg.pad_pre_ms) * FRAME;
            let tail = &b.samples[tail_from..];
            self.pre = tail[tail.len().saturating_sub(keep)..].iter().copied().collect();
            return self.finish(b);
        }
        if b.samples.len() >= frames_in(self.cfg.max_segment_ms) * FRAME {
            let b = self.building.replace(Building { samples: Vec::new(), voiced_frames: 0, voiced_end: 0, silent_run: 0 })?;
            return self.finish(b);
        }
        None
    }

    fn finish(&mut self, mut b: Building) -> Option<Frames16k> {
        if b.voiced_frames < frames_in(self.cfg.min_speech_ms) {
            return None;
        }
        let cut = (b.voiced_end + frames_in(self.cfg.pad_post_ms) * FRAME).min(b.samples.len());
        b.samples.truncate(cut);
        self.heard = Heard::Speech;
        Some(Frames16k::from_vec(b.samples))
    }
}
