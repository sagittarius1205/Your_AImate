//! The microphone through cpal, opened per press and closed on release (Docs/DESIGN_STT.md 3).
//!
//! The audio callback only copies into a lock-free ring: no allocation, no lock, no I/O on the audio
//! thread. The rate change happens on the engine thread when it drains the ring, through the one
//! `Boundary` this stream owns, so it happens exactly once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, SampleFormat, StreamConfig};
use stt_core::audio::{Boundary, CaptureFormat, NativeFormat, Pcm};
use stt_core::engine::{Capture, Chunk, Stream};

/// Two seconds of native audio. The engine drains every 20 ms, and a segment decoded while the
/// button is held stalls it for a few hundred milliseconds, which this covers.
const RING_SECONDS: usize = 2;

pub struct Mic;

struct Device {
    device: cpal::Device,
    name: String,
    config: StreamConfig,
    sample_format: SampleFormat,
}

fn default_device() -> Result<Device, String> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or("no default input device")?;
    let name = device.description().map(|d| d.name().to_string()).unwrap_or_else(|e| format!("(unnamed: {e})"));
    let supported = device.default_input_config().map_err(|e| format!("{name}: no default input format: {e}"))?;
    Ok(Device { device, name, config: supported.config(), sample_format: supported.sample_format() })
}

/// What `ready` reports: the default device and the format it will deliver, without opening it.
pub fn describe() -> Result<CaptureFormat, String> {
    let d = default_device()?;
    let native = NativeFormat { rate: d.config.sample_rate, channels: d.config.channels };
    let boundary = Boundary::new(native).map_err(|e| format!("{}: {e}", d.name))?;
    Ok(CaptureFormat { device: d.name, native, resampled: boundary.resampled() })
}

impl Capture for Mic {
    fn open(&mut self) -> Result<Box<dyn Stream>, String> {
        let d = default_device()?;
        let native = NativeFormat { rate: d.config.sample_rate, channels: d.config.channels };
        let boundary = Boundary::new(native).map_err(|e| format!("{}: {e}", d.name))?;
        let capacity = native.rate as usize * usize::from(native.channels) * RING_SECONDS;
        let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let failed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let xruns = Arc::new(AtomicU64::new(0));
        let on_error = {
            let failed = Arc::clone(&failed);
            let xruns = Arc::clone(&xruns);
            let name = d.name.clone();
            move |e: cpal::Error| {
                // A glitch is not the end of the press. WASAPI reports one as the stream starts
                // (measured on this machine, 2026-11-05), and ending every press on it would make
                // the microphone unusable.
                if matches!(e.kind(), ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied) {
                    xruns.fetch_add(1, Ordering::Relaxed);
                    eprintln!("vrstt: {name}: {e}");
                    return;
                }
                if let Ok(mut slot) = failed.lock() {
                    slot.get_or_insert_with(|| format!("{name}: {e}"));
                }
            }
        };
        let lost = Arc::clone(&dropped);
        let stream = match d.sample_format {
            SampleFormat::F32 => d.device.build_input_stream::<f32, _, _>(
                d.config,
                move |data, _| push(&mut tx, data.iter().copied(), &lost),
                on_error,
                None,
            ),
            SampleFormat::I16 => d.device.build_input_stream::<i16, _, _>(
                d.config,
                move |data, _| push(&mut tx, data.iter().map(|&s| f32::from(s) / 32768.0), &lost),
                on_error,
                None,
            ),
            other => return Err(format!("{}: sample format {other:?} is neither f32 nor i16", d.name)),
        }
        .map_err(|e| format!("{}: cannot open: {e}", d.name))?;
        stream.play().map_err(|e| format!("{}: cannot start: {e}", d.name))?;
        Ok(Box::new(Open { _stream: stream, rx, boundary, dropped, failed, xruns, scratch: Vec::new() }))
    }
}

fn push(tx: &mut rtrb::Producer<f32>, samples: impl Iterator<Item = f32>, dropped: &AtomicU64) {
    let mut lost = 0u64;
    for s in samples {
        if tx.push(s).is_err() {
            lost += 1;
        }
    }
    if lost > 0 {
        dropped.fetch_add(lost, Ordering::Relaxed);
    }
}

struct Open {
    _stream: cpal::Stream,
    rx: rtrb::Consumer<f32>,
    boundary: Boundary,
    dropped: Arc<AtomicU64>,
    failed: Arc<Mutex<Option<String>>>,
    xruns: Arc<AtomicU64>,
    scratch: Vec<f32>,
}

impl Stream for Open {
    fn read(&mut self) -> Result<Chunk, String> {
        if let Some(why) = self.failed.lock().ok().and_then(|slot| slot.clone()) {
            return Err(why);
        }
        self.scratch.clear();
        while let Ok(s) = self.rx.pop() {
            self.scratch.push(s);
        }
        let audio = self.boundary.push(Pcm::F32(&self.scratch));
        // Native samples lost; reported at the native rate because that is where they were lost.
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        let glitches = self.xruns.swap(0, Ordering::Relaxed);
        Ok(Chunk { audio, dropped, glitches })
    }
}
