// How long each piece of an answer is. The first piece should sound as soon as it can, and every
// piece after it has to be synthesised before the audio in front of it runs out, or the character
// stops talking mid-answer (Docs/DESIGN_ChatCore.md 11.1 measured gaps of up to 4.32 s when every
// clause was its own piece).
//
// Synthesis is serial, one model on one GPU, so the only overlap is "piece k+1 synthesises while
// piece k plays". An answer arrives here as the syllable count of each atom (`reply::Atom`), and a
// decision leaves as a number of atoms. The engine owns the clock and hands in what it measured, so
// every decision here can be checked with numbers alone.

/// Every synthesis estimate is this much longer than the pace predicts, so a run the estimate says
/// just fits still has a fifth of its time to spare.
pub const MARGIN: f64 = 1.25;
/// Seconds every piece costs on top of its synthesis: the request, the IPC to the page, the decode
/// there.
pub const OVERHEAD: f64 = 0.2;
/// How far one voiced piece moves the pace toward what it measured. Half: a service that changes
/// speed is followed within a turn, and one odd clip moves the pace only half of the way.
const LEARNING: f64 = 0.5;

/// What the core believes about the speech service. One per engine, learned from every voiced
/// piece, so a slower or faster service paces itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pace {
    /// Seconds of synthesis per second of audio. Above 1 the service is slower than real time.
    pub rate: f64,
    /// Seconds of audio per syllable.
    pub per_syllable: f64,
}

/// What one voiced piece measured.
#[derive(Clone, Copy, Debug)]
pub struct Measured {
    pub syllables: usize,
    /// Seconds of audio that came back.
    pub audio: f64,
    /// Seconds from asking the service to having the audio.
    pub synthesis: f64,
}

impl Pace {
    /// Measured on this machine on 2026-11-06 with the product voice (rate 0.637 to 0.688 per clip,
    /// 0.188 s per syllable plus 1.3 s per clip), rounded toward the slow side.
    pub const START: Pace = Pace { rate: 0.70, per_syllable: 0.21 };

    /// Estimated seconds of audio for `syllables`.
    pub fn sound(&self, syllables: usize) -> f64 {
        self.per_syllable * syllables as f64
    }

    /// Estimated seconds to synthesise `syllables`.
    pub fn synthesis(&self, syllables: usize) -> f64 {
        MARGIN * self.rate * self.sound(syllables) + OVERHEAD
    }

    /// How many atoms go into the first piece: the fewest after which `next` can bring in every
    /// later piece before the audio in front of it runs out. All of them when no shorter first
    /// piece can, and always all of them from a service slower than real time.
    pub fn first(&self, atoms: &[usize]) -> usize {
        if self.rate > 1.0 {
            return atoms.len();
        }
        (1..atoms.len()).find(|&k| self.keeps_time(atoms[..k].iter().sum(), &atoms[k..])).unwrap_or(atoms.len())
    }

    /// How many of the remaining atoms go into the next piece, with `lead` seconds of audio still to
    /// play: the longest run whose synthesis fits in the lead. One atom when even that does not
    /// fit, because the shortest run makes the shortest gap.
    pub fn next(&self, atoms: &[usize], lead: f64) -> usize {
        let fitting = atoms
            .iter()
            .scan(0, |run, &syllables| {
                *run += syllables;
                Some(*run)
            })
            .take_while(|&run| self.synthesis(run) <= lead)
            .count();
        fitting.max(1).min(atoms.len())
    }

    /// Moves the pace toward what a voiced piece measured.
    pub fn learn(&mut self, piece: Measured) {
        if piece.audio > 0.0 {
            self.rate += LEARNING * (piece.synthesis / piece.audio - self.rate);
        }
        // An echo of nothing but punctuation can be voiced and still have no syllable.
        if piece.syllables > 0 {
            self.per_syllable += LEARNING * (piece.audio / piece.syllables as f64 - self.per_syllable);
        }
    }

    /// Whether, after a first piece of `head` syllables, every later seam is made in time when each
    /// later piece is the one `next` picks.
    fn keeps_time(&self, head: usize, mut rest: &[usize]) -> bool {
        // Playback starts when the first piece is sent, so all of it is still to play.
        let mut lead = self.sound(head);
        while !rest.is_empty() {
            let n = self.next(rest, lead);
            let piece: usize = rest[..n].iter().sum();
            let synthesis = self.synthesis(piece);
            if synthesis > lead {
                return false;
            }
            lead += self.sound(piece) - synthesis;
            rest = &rest[n..];
        }
        true
    }
}
