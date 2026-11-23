use crate::chat::{self, ChatEnv};
use crate::claude;
use crate::engine::{self, Config, Engine};
use crate::protocol::{self, Event, Request};
use crate::relay::{self, Courier, Why};
use crate::reply::Relaying;
use crate::scratch::Scratch;
use crate::voice::{Speaker, SpeechEnv};
use std::ffi::OsStr;
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::Duration;

/// Which `claude` model the character runs on when `VRCHATCORE_MODEL` does not say. Sonnet since
/// 2026-11-24: haiku wrote a wrong or Simplified character in 5 of 20 turns, sonnet in none of 6, and
/// sonnet was not slower (Docs/DESIGN_ChatCore.md section 14). The older timings in that document were
/// taken on haiku. The relay's courier keeps its own model, `relay::MODEL`.
pub const DEFAULT_MODEL: &str = "sonnet";
pub const MODEL_ENV: &str = "VRCHATCORE_MODEL";

fn emit(out: &mut impl Write, event: &Event) -> Result<(), String> {
    let mut line = serde_json::to_string(event).map_err(|e| format!("encoding event: {e}"))?;
    line.push('\n');
    out.write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| format!("writing stdout: {e}"))
}

/// What `serve` was asked on its command line.
#[derive(Debug, PartialEq)]
pub struct Options {
    /// `None` is "whichever can be had".
    pub engine: Option<Engine>,
    pub warmup: Duration,
    /// Whether the `relay` op delivers, and the character is offered a way to ask for one.
    pub relay: bool,
    /// How long each of the courier's turns may take. Shorter only for the self-test.
    pub relay_step: Duration,
}

/// `serve [--engine echo|claude|openai] [--warmup-ms N] [--relay] [--relay-step-ms N]`. Strict on
/// purpose: a flag that is misspelled and ignored would be a self-test that runs the engine it
/// meant to avoid.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options { engine: None, warmup: Duration::ZERO, relay: false, relay_step: relay::STEP_TIMEOUT };
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let mut value = |what: &str| args.next().ok_or_else(|| format!("{flag} needs {what}"));
        match flag.as_str() {
            "--engine" => {
                let name = value("echo, claude or openai")?;
                options.engine = Some(Engine::from_name(name).ok_or_else(|| format!("--engine is echo, claude or openai, not {name:?}"))?);
            }
            "--warmup-ms" => {
                let ms = value("a number of milliseconds")?;
                options.warmup = Duration::from_millis(ms.parse().map_err(|_| format!("--warmup-ms needs a whole number, not {ms:?}"))?);
            }
            "--relay" => options.relay = true,
            "--relay-step-ms" => {
                let ms = value("a number of milliseconds")?;
                options.relay_step = Duration::from_millis(ms.parse().map_err(|_| format!("--relay-step-ms needs a whole number, not {ms:?}"))?);
            }
            other => return Err(format!("serve does not take {other:?}; it takes --engine, --warmup-ms, --relay and --relay-step-ms")),
        }
    }
    Ok(options)
}

/// The engine `ENGINE_ENV` names, which is how the pet chooses one. Unset or empty is no choice; a
/// value it does not know stops the core, like a misspelled flag, because a quiet fallback would be
/// a character answering through a service the user did not pick.
pub fn engine_from_env(value: Option<&str>) -> Result<Option<Engine>, String> {
    match value.filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some("claude") => Ok(Some(Engine::Claude)),
        Some("openai") => Ok(Some(Engine::Openai)),
        Some(other) => Err(format!("{} is claude or openai, not {other:?}", chat::ENGINE_ENV)),
    }
}

/// Tells the client when the engine could not take a request, because nothing else will.
fn reported(taken: Result<(), String>) -> Result<(), String> {
    match taken {
        Ok(()) => Ok(()),
        Err(message) => emit(&mut std::io::stdout(), &Event::Error { message }),
    }
}

/// Returns Ok on `shutdown` or when the client closes stdin. Err is for a broken transport, not
/// for a bad request: a bug in the client must not kill the core, so a bad line is answered with
/// an `error` event and the loop goes on.
pub fn run(args: &[String]) -> Result<(), String> {
    let options = parse_args(args)?;
    let engine = match options.engine {
        Some(engine) => Some(engine),
        None => engine_from_env(std::env::var(chat::ENGINE_ENV).ok().as_deref())?,
    };

    // Declared before the engine, so it is dropped after it: the directory cannot be removed
    // while the engine's process is still running in it.
    let scratch = Scratch::under(&std::env::temp_dir());
    let claude = claude::candidates(std::env::var_os(claude::EXE_ENV), std::env::var_os("USERPROFILE"));
    let config = Config {
        engine,
        model: std::env::var(MODEL_ENV).ok().filter(|m| !m.is_empty()).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        claude: claude.clone(),
        chat: ChatEnv::from_process(),
        scratch: scratch.root().to_path_buf(),
        warmup: options.warmup,
        turn_timeout: claude::TURN_TIMEOUT,
        speaker: Speaker::new(SpeechEnv::from_process(), &|name| std::env::var_os(name)),
        relaying: if options.relay { Relaying::On } else { Relaying::Off },
    };
    let ctx = relay::Ctx {
        claude,
        scratch: scratch.root().to_path_buf(),
        // A core that cannot find itself still serves. An empty path is a gate that never starts,
        // which the courier's first call turns into `gate_absent`.
        exe: std::env::current_exe().unwrap_or_default(),
        withheld: [config.speaker.key_env(), config.chat.key_env()].into_iter().flatten().map(OsStr::to_os_string).collect(),
        step_timeout: options.relay_step,
    };

    let mut input = std::io::stdin().lock();
    emit(&mut std::io::stdout(), &Event::Ready { version: env!("CARGO_PKG_VERSION") })?;
    // After `ready` and not before: nothing the engine does, a slow start included, can hold it up.
    // Stdout is not locked for the whole run because the engine writes to it from its own thread.
    let engine = engine::start(config, Box::new(|event| emit(&mut std::io::stdout(), event)));
    // After the engine, so it is dropped before it, and both before the scratch directory.
    let courier = options.relay.then(|| Courier::new(ctx, Arc::new(|event: &Event| emit(&mut std::io::stdout(), event)), engine.noter()));

    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = input
            .read_until(b'\n', &mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        if n == 0 {
            return Ok(());
        }
        match protocol::parse(&buf) {
            Ok(Request::Shutdown) => return Ok(()),
            Ok(Request::Say { text, keep }) => reported(engine.say(text, keep))?,
            Ok(Request::Character(character)) => reported(engine.set_character(character))?,
            Ok(Request::Greet(greeting)) => reported(engine.greet(greeting))?,
            Ok(Request::Relay(order)) => match &courier {
                Some(courier) => courier.start(order),
                None => emit(&mut std::io::stdout(), &relay::refused(order.nonce().clone(), Why::Off, String::new()))?,
            },
            Ok(Request::UnfitRelay { nonce, why }) => {
                let why = if courier.is_some() { (Why::Failed, why) } else { (Why::Off, String::new()) };
                emit(&mut std::io::stdout(), &relay::refused(nonce, why.0, why.1))?;
            }
            Err(message) => {
                eprintln!("vrchatcore: {message}");
                emit(&mut std::io::stdout(), &Event::Error { message })?;
            }
        }
    }
}
