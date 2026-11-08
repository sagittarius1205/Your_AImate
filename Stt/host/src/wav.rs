//! Reads a RIFF WAVE file as the device would deliver it: native rate, interleaved, PCM16 or float32.
//! The `file` mode feeds this through the same `Boundary` as the microphone.

use stt_core::audio::NativeFormat;

pub enum Samples {
    I16(Vec<i16>),
    F32(Vec<f32>),
}

pub struct Wav {
    pub format: NativeFormat,
    pub samples: Samples,
}

pub fn read(bytes: &[u8]) -> Result<Wav, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF WAVE file".to_string());
    }
    let mut at = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes([bytes[at + 4], bytes[at + 5], bytes[at + 6], bytes[at + 7]]) as usize;
        let body = &bytes[at + 8..(at + 8 + len).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            let u16_at = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
            let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            fmt = Some((u16_at(0), u16_at(2), rate, u16_at(14)));
        } else if id == b"data" {
            let (tag, channels, rate, bits) = fmt.ok_or("the data chunk comes before fmt")?;
            let format = NativeFormat { rate, channels };
            // 0xFFFE is WAVE_FORMAT_EXTENSIBLE; its subformat is read off the bit depth.
            let samples = match (tag, bits) {
                (1 | 0xFFFE, 16) => Samples::I16(body.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()),
                (3 | 0xFFFE, 32) => {
                    Samples::F32(body.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
                }
                _ => return Err(format!("format tag {tag} at {bits} bits is neither PCM16 nor float32")),
            };
            return Ok(Wav { format, samples });
        }
        at += 8 + len + (len & 1);
    }
    Err("no data chunk".to_string())
}

/// PCM16 mono, for the files the self-tests write.
pub fn write_pcm16(rate: u32, samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
