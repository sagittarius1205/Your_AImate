//! The STT service's domain: 16 kHz audio, the segmenter and the engine reducer. No operating system
//! and no onnxruntime in here (Docs/DESIGN_STT.md 8), so every rule in it is tested without a device
//! or a model.

#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod audio;
pub mod engine;
pub mod segment;
