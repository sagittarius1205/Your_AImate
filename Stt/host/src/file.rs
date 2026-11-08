//! A WAV file standing in for the microphone (Docs/DESIGN_STT.md 7.3: the benchmark runs the same
//! engine and swaps only the device). The file goes through the same `Boundary`, the same
//! segmenter, VAD and recogniser, and comes out as the same events on the same wire.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use stt_core::audio::{Boundary, Pcm};
use stt_core::engine::{Capture, Chunk, Stream};

use crate::wav::{Samples, Wav};

/// Each open plays the next queued file, 20 ms of native audio per read, the cadence the engine
/// drains a real device at.
#[derive(Default)]
pub struct FileCapture {
    queue: Rc<RefCell<VecDeque<Wav>>>,
    /// Set once the open file's last sample has been handed over, so the driver knows to release.
    done: Rc<Cell<bool>>,
}

impl FileCapture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn queue(&self) -> Rc<RefCell<VecDeque<Wav>>> {
        Rc::clone(&self.queue)
    }

    pub fn done(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.done)
    }
}

impl Capture for FileCapture {
    fn open(&mut self) -> Result<Box<dyn Stream>, String> {
        let wav = self.queue.borrow_mut().pop_front().ok_or("no file is queued")?;
        self.done.set(false);
        let boundary = Boundary::new(wav.format).map_err(|e| e.to_string())?;
        let step = (wav.format.rate as usize / 50) * usize::from(wav.format.channels);
        Ok(Box::new(Playing { wav, boundary, at: 0, step, done: Rc::clone(&self.done) }))
    }
}

struct Playing {
    wav: Wav,
    boundary: Boundary,
    at: usize,
    step: usize,
    done: Rc<Cell<bool>>,
}

impl Stream for Playing {
    fn read(&mut self) -> Result<Chunk, String> {
        let len = match &self.wav.samples {
            Samples::I16(s) => s.len(),
            Samples::F32(s) => s.len(),
        };
        if self.at >= len {
            let audio = if self.done.get() { Default::default() } else { self.boundary.finish() };
            self.done.set(true);
            return Ok(Chunk { audio, dropped: 0, glitches: 0 });
        }
        let end = (self.at + self.step).min(len);
        let audio = match &self.wav.samples {
            Samples::I16(s) => self.boundary.push(Pcm::I16(&s[self.at..end])),
            Samples::F32(s) => self.boundary.push(Pcm::F32(&s[self.at..end])),
        };
        self.at = end;
        Ok(Chunk { audio, dropped: 0, glitches: 0 })
    }
}
