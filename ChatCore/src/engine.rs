// The engine: whatever turns a sentence into an answer. It starts when the core starts, on its own
// thread, and sentences reach it through a queue.
//
// A change of character and a greeting go through the same queue, so the order the pet sent things
// in is the order they happen in, and none of them can land in the middle of a turn: the thread
// does one job at a time, and a turn is one job.
//
// Both halves of "resident and early" live in this file and neither depends on which engine runs.
// `start` returns at once, so the core says `ready` without waiting for the engine. A sentence
// that arrives before the engine is up waits in the queue and is answered when it is, which is not
// a failure. The queue is the channel the thread reads from, and the engine's start-up is the only
// thing between the first sentence and the thread reading it.

use crate::character::{Character, Greeting, VoiceId};
use crate::claude::{self, Failure, Proc, StartError};
use crate::pace::{Measured, Pace};
use crate::protocol::Event;
use crate::reply::{self, Atom};
use crate::scratch;
use crate::voice::{Speaker, Voice};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// Says back what it was told. Kept because automated tests must not spend anything, and
    /// because with no `claude` to be found the character still has something to do.
    Echo,
    Claude,
}

impl Engine {
    pub fn from_name(name: &str) -> Option<Engine> {
        match name {
            "echo" => Some(Engine::Echo),
            "claude" => Some(Engine::Claude),
            _ => None,
        }
    }
}

pub struct Config {
    /// `None` is "whichever can be had": `claude` when it can be found, echo when it cannot.
    pub engine: Option<Engine>,
    pub model: String,
    /// Where `claude` is looked for, best first.
    pub claude: Vec<OsString>,
    /// Holds the engine's working directory. Owned by the caller, who removes it.
    pub scratch: PathBuf,
    /// How long the engine takes to come up, as a delay before it starts. It exists so the queue
    /// can be tested with an engine that is up instantly: a sentence sent into this delay must be
    /// held and not lost.
    pub warmup: Duration,
    pub turn_timeout: Duration,
    /// Gives every turn its voice, whichever engine wrote it.
    pub speaker: Speaker,
}

/// Where the engine's events go. Returns `Err` when they cannot go anywhere (the client closed
/// its end), which ends the engine.
pub type Sink = Box<dyn Fn(&Event) -> Result<(), String> + Send>;

/// How long `close` waits for the engine thread before leaving it.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// What the thread is asked to do, in the order it is asked.
enum Job {
    Say(String),
    /// From the next job on she is this character.
    Character(Character),
    /// She says this in the current voice, and the model is not asked.
    Greet(Greeting),
}

pub struct Handle {
    jobs: Option<Sender<Job>>,
    stop: Arc<AtomicBool>,
    finished: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

/// Starts the engine and returns immediately.
pub fn start(config: Config, sink: Sink) -> Handle {
    let (jobs, queue) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let seen_by_thread = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        work(&config, &queue, &seen_by_thread, &sink);
        let _ = done.send(());
    });
    Handle { jobs: Some(jobs), stop, finished, thread: Some(thread) }
}

impl Handle {
    /// Puts a sentence in the queue. `Err` means the engine thread is gone, which the caller should
    /// say, because nothing else will.
    pub fn say(&self, text: String) -> Result<(), String> {
        self.submit(Job::Say(text))
    }

    /// Queues a change of character. A turn in flight, and any queued before this, is finished as
    /// the character it started as.
    pub fn set_character(&self, character: Character) -> Result<(), String> {
        self.submit(Job::Character(character))
    }

    /// Queues a greeting, which waits for the turn in flight like any other job: it is never
    /// dropped, and it never cuts into a turn's parts.
    pub fn greet(&self, greeting: Greeting) -> Result<(), String> {
        self.submit(Job::Greet(greeting))
    }

    fn submit(&self, job: Job) -> Result<(), String> {
        match &self.jobs {
            Some(jobs) => jobs.send(job).map_err(|_| "the engine has stopped".to_string()),
            None => Err("the engine has been closed".to_string()),
        }
    }

    /// Stops the engine and waits until it has let go of everything it started. A turn in flight
    /// is dropped, not finished: the thread looks at the flag while it waits.
    pub fn close(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.jobs = None;
        let Some(thread) = self.thread.take() else { return };
        match self.finished.recv_timeout(CLOSE_GRACE) {
            Err(RecvTimeoutError::Timeout) => eprintln!("vrchatcore: the engine did not stop within {CLOSE_GRACE:?}"),
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.close();
    }
}

/// What the thread is running. Echo may owe the client one notice about why it is echo.
enum Backend {
    Echo { notice: Option<String> },
    /// `None` between processes: before the first one comes up, and after one is given up on. The
    /// next sentence starts another.
    Claude { proc: Option<Proc> },
}

fn work(config: &Config, queue: &Receiver<Job>, stop: &AtomicBool, sink: &Sink) {
    pause(config.warmup, stop);
    let mut character = Character::standard();
    let mut backend = boot(config, &character);
    // One for the life of the engine, so what one turn learns about the speech service paces the next.
    let mut pace = Pace::START;
    for job in queue {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let sent = match job {
            Job::Say(text) => say(&text, &mut backend, &character, &mut pace, config, stop, sink),
            Job::Greet(greeting) => speak(&reply::echo(greeting.into_text()), &mut pace, &config.speaker, character.voice(), stop, sink),
            Job::Character(next) => {
                change_character(&mut character, next, &mut backend, config);
                Ok(())
            }
        };
        match sent {
            Ok(()) => {}
            Err(Halt::Stopped) => break,
            Err(Halt::Closed(why)) => {
                eprintln!("vrchatcore: engine stopping, {why}");
                break;
            }
        }
    }
    if let Backend::Claude { proc: Some(proc) } = backend {
        proc.close();
    }
}

/// One sentence, answered and sent.
fn say(text: &str, backend: &mut Backend, character: &Character, pace: &mut Pace, config: &Config, stop: &AtomicBool, sink: &Sink) -> Result<(), Halt> {
    let turn = match backend {
        Backend::Echo { .. } => Turn::Said(reply::echo(text.to_string())),
        Backend::Claude { proc } => {
            let began = Instant::now();
            let turn = claude_turn(proc, config, character, text, stop);
            eprintln!("vrchatcore: claude answered in {:.0} ms", began.elapsed().as_secs_f64() * 1000.0);
            turn
        }
    };
    match turn {
        Turn::Said(atoms) => speak(&atoms, pace, &config.speaker, character.voice(), stop, sink),
        Turn::Failed(why) => send(&error(why), stop, sink),
        Turn::Stopped => Err(Halt::Stopped),
    }
    .and_then(|()| match backend {
        // After the echo and not before, so it is the last thing the client shows.
        Backend::Echo { notice } => notice.take().map_or(Ok(()), |message| send(&Event::Error { message }, stop, sink)),
        Backend::Claude { .. } => Ok(()),
    })
}

/// Makes `next` the character. The resident `claude` was started with the previous one's prompt
/// and cannot be told another, so when the prompt differs it is replaced now, between jobs, and
/// not at the next sentence, which would pay for the start. A change of voice alone costs nothing.
fn change_character(current: &mut Character, next: Character, backend: &mut Backend, config: &Config) {
    let new_prompt = reply::prompt(&next) != reply::prompt(current);
    eprintln!(
        "vrchatcore: character is now {} (voice {}){}",
        next.name(),
        next.voice().as_str(),
        if new_prompt { ", and claude is restarted" } else { "" }
    );
    *current = next;
    if let (true, Backend::Claude { proc }) = (new_prompt, backend) {
        if let Some(old) = proc.take() {
            old.close();
        }
        *proc = launch(config, current).map_err(|e| eprintln!("vrchatcore: {}", e.describe())).ok();
    }
}

/// Why a turn stopped sending.
enum Halt {
    /// The core is shutting down. Synthesis takes seconds, and a core asked to leave in the
    /// meantime prints nothing more.
    Stopped,
    /// The client closed its end.
    Closed(String),
}

fn halted(stop: &AtomicBool) -> Result<(), Halt> {
    if stop.load(Ordering::Relaxed) { Err(Halt::Stopped) } else { Ok(()) }
}

fn send(event: &Event, stop: &AtomicBool, sink: &Sink) -> Result<(), Halt> {
    halted(stop)?;
    sink(event).map_err(Halt::Closed)
}

/// Sends an answer in parts, each as soon as its voice is ready, and then `turn_end`. `pace` cuts
/// the parts: the first from its estimates alone, every later one from how much of the audio
/// already sent is still to play, and each voiced part teaches it.
fn speak(atoms: &[Atom], pace: &mut Pace, speaker: &Speaker, voice: &VoiceId, stop: &AtomicBool, sink: &Sink) -> Result<(), Halt> {
    let syllables: Vec<usize> = atoms.iter().map(|atom| atom.syllables).collect();
    // When the page runs out of what it has been sent. Playback is taken to start when part 0 is
    // sent; each later part plays after it, or at once if it arrives after the end.
    let mut playback_end: Option<Instant> = None;
    let mut at = 0;
    let mut part = 0;
    while at < atoms.len() {
        halted(stop)?;
        let rest = &syllables[at..];
        let lead = playback_end.map(|end| end.saturating_duration_since(Instant::now()).as_secs_f64());
        let n = lead.map_or_else(|| pace.first(rest), |lead| pace.next(rest, lead));
        let piece: usize = rest[..n].iter().sum();
        let said = reply::said(&atoms[at..at + n]);
        let began = Instant::now();
        let heard = speaker.voice(&said.spoken(), voice);
        let synthesis = began.elapsed().as_secs_f64();
        let speech = match heard {
            Voice::Voiced(speech) => speech,
            Voice::Silent { why } => {
                // This part and everything after it go out as one silent utterance, and the service
                // is asked nothing more this turn: one that just failed would most likely fail again.
                eprintln!(
                    "vrchatcore: part {part} silent after {:.0} ms, and so is the rest of the turn ({} syllables): {why}",
                    synthesis * 1000.0,
                    rest.iter().sum::<usize>()
                );
                send(&reply::said(&atoms[at..]).utterance(part, Voice::Silent { why }), stop, sink)?;
                break;
            }
        };
        let audio = speech.seconds();
        pace.learn(Measured { syllables: piece, audio, synthesis });
        let sent = Instant::now();
        send(&said.utterance(part, Voice::Voiced(speech)), stop, sink)?;
        let queued = match (lead, playback_end) {
            (Some(lead), Some(end)) => format!("cut with {lead:.2} s of lead, sent with {:+.2} s", seconds_until(end, sent)),
            _ => "first".to_string(),
        };
        eprintln!(
            "vrchatcore: part {part}: {piece} syllables, {audio:.2} s of audio in {:.0} ms, {queued}; pace now {:.3} s per audio s, {:.3} s per syllable",
            synthesis * 1000.0,
            pace.rate,
            pace.per_syllable
        );
        playback_end = Some(playback_end.map_or(sent, |end| end.max(sent)) + Duration::from_secs_f64(audio));
        at += n;
        part += 1;
    }
    send(&Event::TurnEnd, stop, sink)
}

/// Signed: below zero, the page went quiet for that long before `at`.
fn seconds_until(end: Instant, at: Instant) -> f64 {
    match end.checked_duration_since(at) {
        Some(ahead) => ahead.as_secs_f64(),
        None => -at.duration_since(end).as_secs_f64(),
    }
}

/// Waits `delay`, or less if the core is shutting down.
fn pause(delay: Duration, stop: &AtomicBool) {
    let until = Instant::now() + delay;
    while !stop.load(Ordering::Relaxed) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        thread::sleep(left.min(Duration::from_millis(10)));
    }
}

fn boot(config: &Config, character: &Character) -> Backend {
    match config.engine {
        Some(Engine::Echo) => {
            eprintln!("vrchatcore: engine echo");
            Backend::Echo { notice: None }
        }
        Some(Engine::Claude) => Backend::Claude { proc: launch(config, character).map_err(|e| eprintln!("vrchatcore: {}", e.describe())).ok() },
        None => match launch(config, character) {
            Ok(proc) => Backend::Claude { proc: Some(proc) },
            Err(error @ StartError::NotFound(_)) => {
                let notice = format!(
                    "{}, so for now the character can only repeat what is said. Put claude on PATH, or set {} to its path",
                    error.describe(),
                    claude::EXE_ENV
                );
                eprintln!("vrchatcore: engine echo, because {}", error.describe());
                Backend::Echo { notice: Some(notice) }
            }
            Err(error) => {
                eprintln!("vrchatcore: {}", error.describe());
                Backend::Claude { proc: None }
            }
        },
    }
}

fn launch(config: &Config, character: &Character) -> Result<Proc, StartError> {
    let cwd = scratch::engine_dir(&config.scratch)
        .map_err(|e| StartError::Failed(format!("could not make a working directory for claude under {}: {e}", config.scratch.display())))?;
    let proc = claude::start(&config.claude, &cwd, &config.model, character, config.speaker.key_env())?;
    eprintln!("vrchatcore: engine claude started, model {}, working in {}", config.model, cwd.display());
    Ok(proc)
}

fn error(message: String) -> Event {
    Event::Error { message }
}

/// What an engine made of one sentence.
enum Turn {
    Said(Vec<Atom>),
    /// The reason, which the client is told as an `error`.
    Failed(String),
    /// The core is shutting down, so nothing is said.
    Stopped,
}

/// One sentence through `claude`.
fn claude_turn(slot: &mut Option<Proc>, config: &Config, character: &Character, text: &str, stop: &AtomicBool) -> Turn {
    // A process that left while nobody was talking to it has not eaten this sentence yet, so it is
    // replaced without making the sentence pay for it.
    if slot.as_mut().is_some_and(|proc| proc.exited().is_some()) {
        eprintln!("vrchatcore: claude had exited while idle, starting another");
        if let Some(proc) = slot.take() {
            proc.close();
        }
    }
    if slot.is_none() {
        match launch(config, character) {
            Ok(proc) => *slot = Some(proc),
            Err(e) => return Turn::Failed(e.describe()),
        }
    }
    let Some(proc) = slot.as_mut() else {
        return Turn::Failed("claude is not running".to_string());
    };
    match proc.ask(text, config.turn_timeout, stop) {
        Ok(message) => match claude::answer(&message) {
            Ok(reply) => Turn::Said(reply.atoms()),
            Err(why) => Turn::Failed(why),
        },
        Err(Failure::Stopped) => Turn::Stopped,
        Err(Failure::Broken(why)) => {
            if let Some(proc) = slot.take() {
                proc.close();
            }
            Turn::Failed(why)
        }
    }
}
