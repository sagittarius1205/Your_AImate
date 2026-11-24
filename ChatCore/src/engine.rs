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
use crate::chat::{self, ChatEnv, History};
use crate::claude::{self, Failure, Proc, StartError};
use crate::dialogue::{Answer, Heard, Keep, Log, Outcome, Took, Who};
use crate::pace::{self, Measured, Pace};
use crate::protocol::Event;
use crate::relay::Fact;
use crate::reply::{self, Atom, Clause, RelayAsk, Relaying, Reply};
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
    /// An OpenAI-compatible service over HTTP (`chat.rs`).
    Openai,
}

impl Engine {
    pub fn from_name(name: &str) -> Option<Engine> {
        match name {
            "echo" => Some(Engine::Echo),
            "claude" => Some(Engine::Claude),
            "openai" => Some(Engine::Openai),
            _ => None,
        }
    }
}

pub struct Config {
    /// `None` is "whichever can be had": `claude` when it can be found, echo when it cannot.
    pub engine: Option<Engine>,
    /// `claude`'s model.
    pub model: String,
    /// Where `claude` is looked for, best first.
    pub claude: Vec<OsString>,
    /// The HTTP engine's service, read only when that engine runs.
    pub chat: ChatEnv,
    /// Holds the engine's working directory. Owned by the caller, who removes it.
    pub scratch: PathBuf,
    /// How long the engine takes to come up, as a delay before it starts. It exists so the queue
    /// can be tested with an engine that is up instantly: a sentence sent into this delay must be
    /// held and not lost.
    pub warmup: Duration,
    pub turn_timeout: Duration,
    /// Gives every turn its voice, whichever engine wrote it.
    pub speaker: Speaker,
    /// Whether the character is offered a way to ask for a relay. Only the `claude` engine is
    /// offered it for now: what a local model makes of the rule has not been measured.
    pub relaying: Relaying,
}

/// Where the engine's events go. Returns `Err` when they cannot go anywhere (the client closed
/// its end), which ends the engine.
pub type Sink = Box<dyn Fn(&Event) -> Result<(), String> + Send>;

/// How long `close` waits for the engine thread before leaving it.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// What the thread is asked to do, in the order it is asked.
enum Job {
    Say(Heard),
    /// From the next job on she is this character.
    Character(Character),
    /// She says this in the current voice, and the model is not asked.
    Greet(Greeting),
    /// How the last relay ended, for the next sentence.
    Relayed(Fact),
}

/// What the courier holds to tell the character how a relay ended. It can queue nothing else.
#[derive(Clone)]
pub struct Noter(Sender<Job>);

impl Noter {
    pub fn note(&self, fact: Fact) {
        let _ = self.0.send(Job::Relayed(fact));
    }
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
    /// Puts a sentence in the queue, timed from now. `Err` means the engine thread is gone, which
    /// the caller should say, because nothing else will.
    pub fn say(&self, text: String, keep: Option<Keep>) -> Result<(), String> {
        self.submit(Job::Say(Heard::now(text, keep)))
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

    /// `None` once the engine has been closed.
    pub fn noter(&self) -> Option<Noter> {
        self.jobs.as_ref().map(|jobs| Noter(jobs.clone()))
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
    /// The service remembers nothing, so the conversation so far is held here.
    Openai { client: chat::Client, history: History },
}

fn work(config: &Config, queue: &Receiver<Job>, stop: &AtomicBool, sink: &Sink) {
    pause(config.warmup, stop);
    let mut character = Character::standard();
    let mut backend = boot(config, &character);
    // One for the life of the engine, so what one turn learns about the speech service paces the next.
    let mut pace = Pace::START;
    // How the last relay ended, put in front of each sentence until a model has answered one.
    let mut note: Option<Fact> = None;
    let mut log = Log::new();
    for job in queue {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let sent = match job {
            Job::Say(heard) => {
                // The record keeps the user's words, so the note goes on a copy.
                let text = match (note, &backend) {
                    (Some(fact), Backend::Claude { .. } | Backend::Openai { .. }) => format!("{}\n{}", reply::note(fact), heard.text),
                    _ => heard.text.clone(),
                };
                let took = say(&text, &mut backend, &character, &mut pace, config, stop, sink);
                if took.as_ref().is_ok_and(|took| took.outcome.answered()) {
                    note = None;
                }
                log.turn(&heard, took.as_ref().ok(), who(&backend, config));
                took.map(|_| ())
            }
            Job::Relayed(fact) => {
                note = Some(fact);
                Ok(())
            }
            Job::Greet(greeting) => {
                let spoke = speak(&reply::echo(greeting.into_text()), None, &mut pace, &config.speaker, character.voice(), stop, sink);
                log.greeted();
                spoke.map(|_| ())
            }
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

/// One sentence, answered and sent, and what it came to. An outcome that is not `answered()` is a
/// sentence the client has been told was not answered, and the engine may not have heard it.
fn say(text: &str, backend: &mut Backend, character: &Character, pace: &mut Pace, config: &Config, stop: &AtomicBool, sink: &Sink) -> Result<Took, Halt> {
    let (turn, engine) = match backend {
        Backend::Echo { .. } => (Turn::Said { atoms: reply::echo(text.to_string()), answer: Answer::echo(text), opening: None }, Instant::now()),
        Backend::Claude { proc } => {
            let began = Instant::now();
            let (turn, answered) = claude_turn(proc, config, character, text, pace, stop, began);
            eprintln!("vrchatcore: claude answered in {:.0} ms", ms(answered.saturating_duration_since(began)));
            (turn, answered)
        }
        Backend::Openai { client, history } => {
            let began = Instant::now();
            let sent = history.len();
            let turn = openai_turn(client, history, character, text, stop);
            eprintln!(
                "vrchatcore: openai answered in {:.0} ms ({sent} turns of history sent{})",
                ms(began.elapsed()),
                match &turn {
                    Turn::Refused(why) | Turn::Failed(why) => format!(", failed: {why}"),
                    _ => String::new(),
                }
            );
            (turn, Instant::now())
        }
    };
    let unspoken = |sent: Result<(), Halt>| sent.map(|()| Spoke { first: None, audio: 0.0 });
    let (outcome, answer, spoke) = match turn {
        Turn::Said { atoms, answer, opening } => (Outcome::Answered, Some(answer), speak(&atoms, opening, pace, &config.speaker, character.voice(), stop, sink)),
        // Her clauses are dropped: nothing she wrote this turn is said, so nothing she wrote can
        // claim an outcome. The pet answers in fixed words.
        Turn::Relay(ask) => (
            Outcome::RelayAsked,
            None,
            unspoken(
                send(&Event::RelayAsked { to: ask.to().to_string(), text: ask.text().to_string() }, stop, sink)
                    .and_then(|()| send(&Event::TurnEnd, stop, sink)),
            ),
        ),
        Turn::Refused(why) => (Outcome::Refused, None, unspoken(send(&error(why), stop, sink))),
        Turn::Failed(why) => (Outcome::Failed, None, unspoken(send(&error(why), stop, sink))),
        Turn::Stopped => return Err(Halt::Stopped),
    };
    let spoke = spoke?;
    match backend {
        // After the echo and not before, so it is the last thing the client shows.
        Backend::Echo { notice } => notice.take().map_or(Ok(()), |message| send(&Event::Error { message }, stop, sink))?,
        Backend::Claude { .. } | Backend::Openai { .. } => {}
    }
    Ok(Took { outcome, answer, engine, first_part: spoke.first, audio: spoke.audio })
}

/// The engine that is running, for the record.
fn who<'a>(backend: &'a Backend, config: &'a Config) -> Who<'a> {
    match backend {
        Backend::Echo { .. } => Who { kind: "echo", model: None },
        Backend::Claude { .. } => Who { kind: "claude", model: Some(&config.model) },
        Backend::Openai { client, .. } => Who { kind: "openai", model: client.service().ok().map(chat::Service::model) },
    }
}

/// Makes `next` the character. A new prompt is someone else, who does not remember what the last
/// one was told. The resident `claude` was started with the previous prompt and cannot be told
/// another, so it is replaced now, between jobs, and not at the next sentence, which would pay for
/// the start; the HTTP engine's history is emptied. A change of voice alone is the same character
/// and costs nothing.
fn change_character(current: &mut Character, next: Character, backend: &mut Backend, config: &Config) {
    let new_prompt = reply::prompt(&next, config.relaying) != reply::prompt(current, config.relaying);
    let what = match backend {
        Backend::Claude { .. } => ", and claude is restarted",
        Backend::Openai { .. } => ", and the history is cleared",
        Backend::Echo { .. } => "",
    };
    eprintln!("vrchatcore: character is now {} (voice {}){}", next.name(), next.voice().as_str(), if new_prompt { what } else { "" });
    *current = next;
    if !new_prompt {
        return;
    }
    match backend {
        Backend::Claude { proc } => {
            if let Some(old) = proc.take() {
                old.close();
            }
            *proc = launch(config, current).map_err(|e| eprintln!("vrchatcore: {}", e.describe())).ok();
        }
        Backend::Openai { history, .. } => history.clear(),
        Backend::Echo { .. } => {}
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

/// When the first part went out, and how much voiced audio went in all.
struct Spoke {
    first: Option<Instant>,
    audio: f64,
}

/// Part 0 voiced while the rest of the answer was still being written: the head of the first
/// clause, said as `speak` would say it. Nothing of it is sent until the whole answer has passed
/// `parse` and `speak` has judged it against that answer.
struct Opening {
    atoms: Vec<Atom>,
    voice: Voice,
    /// Seconds the service took for it.
    synthesis: f64,
}

/// Voices the opening of `clause`, on this thread, while the reader takes in the rest of the answer.
/// A service slower than real time is not asked: it never splits an answer, so its first part is
/// the whole answer and has to wait for it.
fn prepare(clause: &Clause, pace: &Pace, speaker: &Speaker, voice: &VoiceId) -> Option<Opening> {
    if pace.rate > 1.0 {
        eprintln!("vrchatcore: no opening, the speech service is slower than real time");
        return None;
    }
    let mut atoms = clause.atoms();
    let syllables: Vec<usize> = atoms.iter().map(|atom| atom.syllables).collect();
    atoms.truncate(pace::head(&syllables));
    let began = Instant::now();
    let voice = speaker.voice(&reply::said(&atoms).spoken(), voice);
    Some(Opening { atoms, voice, synthesis: began.elapsed().as_secs_f64() })
}

/// What becomes of an opening once the answer is whole, and the word the part 0 line says it with.
/// It is part 0 when the answer begins with exactly its atoms, whatever pause that leaves in front
/// of the piece after it. One the service could not voice is the turn's silence, as a first part the
/// service refuses always was, whatever the answer turned out to be.
fn judge(opening: Option<Opening>, atoms: &[Atom]) -> (Option<Opening>, &'static str) {
    let Some(opening) = opening else { return (None, "none") };
    match &opening.voice {
        Voice::Silent { .. } => (Some(opening), "silent"),
        Voice::Voiced(_) if !atoms.starts_with(&opening.atoms) => (None, "dropped:changed"),
        Voice::Voiced(_) => (Some(opening), "reused"),
    }
}

/// Sends an answer in parts, each as soon as its voice is ready, and then `turn_end`. `pace` cuts
/// the parts: the first from its estimates alone, every later one from how much of the audio
/// already sent is still to play, and each voiced part teaches it. An `opening` that `judge` keeps
/// is part 0, and teaches the pace once, with the time its synthesis really took.
fn speak(atoms: &[Atom], opening: Option<Opening>, pace: &mut Pace, speaker: &Speaker, voice: &VoiceId, stop: &AtomicBool, sink: &Sink) -> Result<Spoke, Halt> {
    let mut spoke = Spoke { first: None, audio: 0.0 };
    let syllables: Vec<usize> = atoms.iter().map(|atom| atom.syllables).collect();
    let (mut ready, verdict) = judge(opening, atoms);
    // When the page runs out of what it has been sent. Playback is taken to start when part 0 is
    // sent; each later part plays after it, or at once if it arrives after the end.
    let mut playback_end: Option<Instant> = None;
    let mut at = 0;
    let mut part = 0;
    while at < atoms.len() {
        halted(stop)?;
        let rest = &syllables[at..];
        let lead = playback_end.map(|end| end.saturating_duration_since(Instant::now()).as_secs_f64());
        let (n, heard, synthesis) = match ready.take() {
            Some(opening) => (opening.atoms.len(), opening.voice, opening.synthesis),
            None => {
                let n = lead.map_or_else(|| pace.first(rest), |lead| pace.next(rest, lead));
                let began = Instant::now();
                let heard = speaker.voice(&reply::said(&atoms[at..at + n]).spoken(), voice);
                (n, heard, began.elapsed().as_secs_f64())
            }
        };
        let opened = if part == 0 { format!("; opening={verdict}") } else { String::new() };
        let speech = match heard {
            Voice::Voiced(speech) => speech,
            Voice::Silent { why } => {
                // This part and everything after it go out as one silent utterance, and the service
                // is asked nothing more this turn: one that just failed would most likely fail again.
                eprintln!(
                    "vrchatcore: part {part} silent after {:.0} ms, and so is the rest of the turn ({} syllables{opened}): {why}",
                    synthesis * 1000.0,
                    rest.iter().sum::<usize>()
                );
                send(&reply::said(&atoms[at..]).utterance(part, Voice::Silent { why }), stop, sink)?;
                spoke.first.get_or_insert_with(Instant::now);
                break;
            }
        };
        let piece: usize = rest[..n].iter().sum();
        let said = reply::said(&atoms[at..at + n]);
        let audio = speech.seconds();
        pace.learn(Measured { syllables: piece, audio, synthesis });
        let sent = Instant::now();
        send(&said.utterance(part, Voice::Voiced(speech)), stop, sink)?;
        spoke.first.get_or_insert(sent);
        spoke.audio += audio;
        let queued = match (lead, playback_end) {
            (Some(lead), Some(end)) => format!("cut with {lead:.2} s of lead, sent with {:+.2} s", seconds_until(end, sent)),
            _ => "first".to_string(),
        };
        eprintln!(
            "vrchatcore: part {part}: {piece} syllables, {audio:.2} s of audio in {:.0} ms, {queued}; pace now {:.3} s per audio s, {:.3} s per syllable{opened}",
            synthesis * 1000.0,
            pace.rate,
            pace.per_syllable
        );
        playback_end = Some(playback_end.map_or(sent, |end| end.max(sent)) + Duration::from_secs_f64(audio));
        at += n;
        part += 1;
    }
    send(&Event::TurnEnd, stop, sink).map(|()| spoke)
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
        Some(Engine::Openai) => {
            let client = chat::Client::new(&config.chat, &|name| std::env::var_os(name), config.turn_timeout);
            match client.service() {
                Ok(service) => eprintln!("vrchatcore: engine openai, model {} at {}", service.model(), service.url()),
                Err(why) => eprintln!("vrchatcore: engine openai cannot run, and every sentence will say why: {why}"),
            }
            Backend::Openai { client, history: History::default() }
        }
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
    let withheld: Vec<&std::ffi::OsStr> = [config.speaker.key_env(), config.chat.key_env()].into_iter().flatten().collect();
    let proc = claude::start(&config.claude, &cwd, &config.model, character, config.relaying, &withheld)?;
    eprintln!("vrchatcore: engine claude started, model {}, working in {}", config.model, cwd.display());
    Ok(proc)
}

fn error(message: String) -> Event {
    Event::Error { message }
}

/// What an engine made of one sentence.
enum Turn {
    /// `opening` is part 0 voiced while the answer was being written, for `speak` to judge. Only a
    /// turn that is said carries one, so a relay, a refusal or a failure drops it unheard.
    Said { atoms: Vec<Atom>, answer: Answer, opening: Option<Opening> },
    /// She asked for a relay, and said nothing that will be heard.
    Relay(RelayAsk),
    /// The engine answered and `reply::parse` turned it down. The reason is told as an `error`.
    Refused(String),
    /// No answer came back that could be parsed. The reason is told as an `error`.
    Failed(String),
    /// The core is shutting down, so nothing is said.
    Stopped,
}

fn said(reply: &Reply, opening: Option<Opening>) -> Turn {
    Turn::Said { atoms: reply.atoms(), answer: Answer::of(reply), opening }
}

/// One sentence through the HTTP engine. The turn joins the history only once `parse` has taken
/// the answer.
fn openai_turn(client: &chat::Client, history: &mut History, character: &Character, text: &str, stop: &AtomicBool) -> Turn {
    match client.ask(&reply::prompt(character, Relaying::Off), history, text, stop) {
        Ok(answered) => match reply::parse(&answered, Relaying::Off) {
            Ok(reply) => {
                history.push(text, &answered);
                said(&reply, None)
            }
            Err(why) => Turn::Refused(why),
        },
        Err(Failure::Stopped) => Turn::Stopped,
        Err(Failure::Broken(why)) => Turn::Failed(why),
    }
}

fn ms(time: Duration) -> f64 {
    time.as_secs_f64() * 1000.0
}

/// One sentence through `claude`, asked at `began`, and when its answer or its failure came back:
/// the instant the reader read `result`, because the engine may have been busy voicing the opening
/// when it arrived. `pace` is what `prepare` asks whether the service can keep up.
fn claude_turn(slot: &mut Option<Proc>, config: &Config, character: &Character, text: &str, pace: &Pace, stop: &AtomicBool, began: Instant) -> (Turn, Instant) {
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
            Err(e) => return (Turn::Failed(e.describe()), Instant::now()),
        }
    }
    let Some(proc) = slot.as_mut() else {
        return (Turn::Failed("claude is not running".to_string()), Instant::now());
    };
    let mut opening = None;
    let mut first = |heard: Result<Clause, String>, read: Instant| match heard {
        Ok(clause) => {
            eprintln!("vrchatcore: first clause closed at {:.0} ms", ms(read.saturating_duration_since(began)));
            opening = prepare(&clause, pace, &config.speaker, character.voice());
            if let Some(ready) = &opening {
                let syllables: usize = ready.atoms.iter().map(|atom| atom.syllables).sum();
                let voiced = match &ready.voice {
                    Voice::Voiced(speech) => format!("{:.2} s of audio", speech.seconds()),
                    Voice::Silent { why } => format!("silent ({why})"),
                };
                eprintln!("vrchatcore: opening ready at {:.0} ms: {syllables} syllables, {voiced}, in {:.0} ms", ms(began.elapsed()), ready.synthesis * 1000.0);
            }
        }
        Err(why) => eprintln!("vrchatcore: no first clause from the stream: {why}"),
    };
    let asked = proc.ask(text, config.turn_timeout, stop, &mut first);
    let unsaid = |why: &str| {
        if opening.is_some() {
            eprintln!("vrchatcore: nothing of this turn is said; opening=dropped:{why}");
        }
    };
    match asked {
        Ok((message, read)) => {
            let (turn, outcome) = match claude::result(&message).map(|text| reply::parse(text, config.relaying)) {
                Ok(Ok(Reply { relay: Some(ask), .. })) => (Turn::Relay(ask), "relay"),
                Ok(Ok(reply)) => return (said(&reply, opening), read),
                Ok(Err(why)) => (Turn::Refused(why), "refused"),
                Err(why) => (Turn::Failed(why), "failed"),
            };
            unsaid(outcome);
            (turn, read)
        }
        Err(Failure::Stopped) => (Turn::Stopped, Instant::now()),
        Err(Failure::Broken(why)) => {
            unsaid("failed");
            if let Some(proc) = slot.take() {
                proc.close();
            }
            (Turn::Failed(why), Instant::now())
        }
    }
}
