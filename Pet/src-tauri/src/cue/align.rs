//! When each character of a reply is said, from the reply's text and its audio alone.
//!
//! A port of the first half of `VRGesturePlanner::Plan` (Source/VRPhysProto/VRGesturePlan.cpp,
//! lines 155-300 and the helpers at 38-90). The speech engine gives no word timings, so the text is
//! lined up against the pauses in the voice: one clause per stretch of speech, characters spread
//! evenly inside it by how many syllables each one is. Chinese says one syllable per character,
//! which is what makes spreading them evenly close enough for a gesture.
//!
//! Every constant is the C++ one, with its line. Arithmetic is `f32` where the C++ is `float`, so
//! the two agree to the bit on the same input, and `--selftest=pet_gesture` checks a hand-computed
//! case against this file.
//!
//! One difference by construction: the C++ indexes UTF-16 code units and this indexes `char`s. The
//! two agree for every character in the Basic Multilingual Plane, which is all of Chinese and every
//! punctuation mark the planner knows; an emoji counts as two characters there and one here.

/// VRGesturePlan.cpp:29. Loudness is measured over 20 ms frames.
pub const FRAME_SECONDS: f32 = 0.02;
/// VRGesturePlan.cpp:30. A frame is speech when it is louder than this share of the loudest frame.
const VOICED_SHARE: f32 = 0.08;
/// VRGesturePlan.cpp:32. A quiet run has to be this long to count as a pause.
const MIN_PAUSE: f32 = 0.04;
/// VRGesturePlan.cpp:35. How far from where the syllable count puts a clause boundary a pause may
/// be and still be taken for it.
const BOUNDARY_SEARCH: f32 = 0.6;

/// VRGesturePlan.cpp:40.
pub fn is_punctuation(c: char) -> bool {
    "，。？！、；：,.?!;:…".contains(c)
}

/// VRGesturePlan.cpp:45.
pub fn ends_sentence(c: char) -> bool {
    "。？！.?!".contains(c)
}

/// VRGesturePlan.cpp:49-64. How much of a syllable a character is: a Chinese character is one, a
/// letter about a third, a space a tenth, punctuation nothing.
fn syllables_in(c: char) -> f32 {
    if is_punctuation(c) {
        0.0
    } else if c as u32 >= 0x2E80 {
        1.0
    } else if c.is_alphanumeric() {
        0.28
    } else if c.is_whitespace() {
        0.1
    } else {
        0.0
    }
}

/// A `[name]` in the text, before the spoken character at index `at`.
#[derive(Clone, PartialEq, Debug)]
pub struct Tag {
    pub at: usize,
    pub name: String,
}

/// VRGesturePlan.cpp:72-90. The text with every `[...]` taken out, and the tags it held. A `[` with
/// no `]` after it is said as it is.
pub fn strip_tags(text: &str) -> (Vec<char>, Vec<Tag>) {
    let chars: Vec<char> = text.chars().collect();
    let mut said = Vec::with_capacity(chars.len());
    let mut tags = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(close) = chars[i..].iter().position(|&c| c == ']').map(|k| i + k) {
                let name: String = chars[i + 1..close].iter().collect::<String>().to_lowercase();
                tags.push(Tag { at: said.len(), name: name.trim().to_string() });
                i = close + 1;
                continue;
            }
        }
        said.push(chars[i]);
        i += 1;
    }
    (said, tags)
}

/// The text lined up against the voice.
#[derive(Clone, Debug)]
pub struct Alignment {
    /// What is actually said: the text without its tags.
    pub said: Vec<char>,
    pub tags: Vec<Tag>,
    /// Each clause as `[start, end)` into `said`, the end past its punctuation.
    pub spans: Vec<(usize, usize)>,
    /// Where in the audio each clause is said, in seconds from the first sample.
    pub clause_times: Vec<(f32, f32)>,
    /// When each character of `said` is said; one more entry, for the end of the speech.
    pub char_time: Vec<f32>,
    /// RMS of each whole 20 ms frame, and the loudest of them.
    pub loudness: Vec<f32>,
    pub peak: f32,
}

/// VRGesturePlan.cpp:162-195. Runs of text up to and including the punctuation that ends them,
/// kept only if something in them is said.
fn clause_spans(said: &[char]) -> Vec<(usize, usize)> {
    let weight = |from: usize, to: usize| said[from..to].iter().map(|&c| syllables_in(c)).sum::<f32>();
    let mut spans = Vec::new();
    let mut start = 0;
    let mut i = 0;
    loop {
        if i < said.len() && !is_punctuation(said[i]) {
            i += 1;
            continue;
        }
        let mut end = i;
        while end < said.len() && is_punctuation(said[end]) {
            end += 1;
        }
        if weight(start, end) > 0.0 {
            spans.push((start, end));
        }
        start = end;
        if i == said.len() {
            return spans;
        }
        i = end;
    }
}

/// VRGesturePlan.cpp:202-212. RMS of each whole frame; a partial last frame has none.
fn frame_loudness(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    let frame = ((sample_rate as f32 * FRAME_SECONDS).round() as usize).max(1);
    samples
        .chunks_exact(frame)
        .map(|chunk| {
            let sum: f64 = chunk.iter().map(|&s| f64::from(s * s)).sum();
            (sum / frame as f64).sqrt() as f32
        })
        .collect()
}

/// Lines `text` up against `samples` (mono, -1..1). `None` where the planner returns an empty plan:
/// nothing in the text is said, or nothing in the audio is louder than silence.
pub fn align(text: &str, samples: &[f32], sample_rate: u32) -> Option<Alignment> {
    let (said, tags) = strip_tags(text);
    let spans = clause_spans(&said);
    if spans.is_empty() || sample_rate == 0 {
        return None;
    }

    // The voice: which frames are speech, and the pauses between (VRGesturePlan.cpp:201-243).
    let loudness = frame_loudness(samples, sample_rate);
    let peak = loudness.iter().copied().fold(0.0f32, f32::max);
    let threshold = peak * VOICED_SHARE;
    let voiced: Vec<usize> = (0..loudness.len()).filter(|&f| loudness[f] > threshold).collect();
    let (&first, &last) = (voiced.first()?, voiced.last()?);
    if peak <= 0.0 {
        return None;
    }
    let mut pauses: Vec<(f32, f32)> = Vec::new();
    let mut f = first;
    while f <= last {
        if loudness[f] > threshold {
            f += 1;
            continue;
        }
        let from = f;
        while f <= last && loudness[f] <= threshold {
            f += 1;
        }
        if (f - from) as f32 * FRAME_SECONDS >= MIN_PAUSE {
            pauses.push((from as f32 * FRAME_SECONDS, f as f32 * FRAME_SECONDS));
        }
        // The frame that ended the run is voiced, and the C++ loop steps past it.
        f += 1;
    }

    // Clause boundaries (VRGesturePlan.cpp:245-284). Each one is expected where its share of the
    // syllables still to come puts it, and taken at the longest pause near there. Not simply the
    // longest pauses: after a comma the voice may pause for less than inside a word.
    let weights: Vec<f32> =
        spans.iter().map(|&(from, to)| said[from..to].iter().map(|&c| syllables_in(c)).sum()).collect();
    let speech_end = (last + 1) as f32 * FRAME_SECONDS;
    let mut from = first as f32 * FRAME_SECONDS;
    let mut clause_times = Vec::with_capacity(spans.len());
    for k in 0..spans.len() {
        let (mut to, mut next) = (speech_end, speech_end);
        if k + 1 < spans.len() {
            let remaining: f32 = weights[k..].iter().sum();
            let expected = from + (speech_end - from) * weights[k] / remaining;
            let mut best: Option<(f32, f32)> = None;
            for &(start, end) in &pauses {
                if start > from
                    && (start - expected).abs() <= BOUNDARY_SEARCH
                    && best.is_none_or(|(bs, be)| end - start > be - bs)
                {
                    best = Some((start, end));
                }
            }
            (to, next) = best.unwrap_or((expected, expected));
        }
        clause_times.push((from, to));
        from = next;
    }

    // When each character is said: spread evenly by syllables across its clause
    // (VRGesturePlan.cpp:286-300).
    let mut char_time = vec![0.0f32; said.len() + 1];
    for (k, &(start, end)) in spans.iter().enumerate() {
        let (t0, t1) = clause_times[k];
        let mut before = 0.0f32;
        for c in start..end {
            let w = syllables_in(said[c]);
            char_time[c] = t0 + (t1 - t0) * (before + 0.5 * w) / weights[k];
            before += w;
        }
    }
    char_time[said.len()] = speech_end;

    Some(Alignment { said, tags, spans, clause_times, char_time, loudness, peak })
}
