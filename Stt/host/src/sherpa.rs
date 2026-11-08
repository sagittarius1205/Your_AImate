//! sherpa-onnx behind the core's two traits: SenseVoice as the `Recognizer`, Silero as the `Vad`.
//!
//! SenseVoice is offline: it decodes a whole segment after the segmenter closes it, so there is no
//! recogniser endpointing to turn off. Release is the only end of an utterance (Docs/DESIGN_STT.md
//! 2.3), which this gets by construction.

use sherpa_onnx::{
    OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, SileroVadModelConfig, VadModelConfig,
    VoiceActivityDetector,
};
use stt_core::audio::{Frames16k, SAMPLE_RATE};
use stt_core::engine::Recognizer;
use stt_core::segment::Vad;

use crate::pack::Verified;

pub const MODEL_NAME: &str = "sense-voice-zh-en-ja-ko-yue-int8-2024-07-17";
pub const VAD_NAME: &str = "silero_vad";

pub struct SenseVoice(OfflineRecognizer);

impl SenseVoice {
    pub fn load(files: &Verified, threads: i32) -> Result<Self, String> {
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(files.asr_model.display().to_string()),
            language: Some("auto".to_string()),
            use_itn: true,
        };
        config.model_config.tokens = Some(files.asr_tokens.display().to_string());
        config.model_config.num_threads = threads;
        config.model_config.provider = Some("cpu".to_string());
        OfflineRecognizer::create(&config)
            .map(SenseVoice)
            .ok_or_else(|| "sherpa-onnx refused the SenseVoice configuration; its reason is on stderr".to_string())
    }
}

impl Recognizer for SenseVoice {
    fn recognize(&mut self, speech: &Frames16k) -> Result<String, String> {
        let stream = self.0.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, &speech.to_f32());
        self.0.decode(&stream);
        stream.get_result().map(|r| r.text).ok_or_else(|| "SenseVoice returned no result".to_string())
    }
}

pub struct Silero(VoiceActivityDetector);

impl Silero {
    pub fn load(files: &Verified) -> Result<Self, String> {
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(files.vad.display().to_string()),
                threshold: 0.5,
                // Short on purpose: the segmenter owns the timing rules, and Silero's own
                // hysteresis would be a second set of thresholds nobody tests.
                min_silence_duration: 0.1,
                min_speech_duration: 0.05,
                window_size: 512,
                max_speech_duration: 60.0,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            provider: Some("cpu".to_string()),
            debug: false,
            ..VadModelConfig::default()
        };
        VoiceActivityDetector::create(&config, 30.0)
            .map(Silero)
            .ok_or_else(|| "sherpa-onnx refused the Silero VAD configuration; its reason is on stderr".to_string())
    }
}

impl Vad for Silero {
    fn voiced(&mut self, frame: &[i16]) -> bool {
        let f: Vec<f32> = frame.iter().map(|&s| f32::from(s) / 32768.0).collect();
        self.0.accept_waveform(&f);
        let voiced = self.0.detected();
        // The segments it queues are not used; the segmenter keeps its own audio.
        self.0.clear();
        voiced
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}
