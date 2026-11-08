use crate::claude;
use crate::engine::{self, Config, Engine};
use crate::protocol::{self, Event, Request};
use crate::scratch::Scratch;
use crate::voice::{Speaker, SpeechEnv};
use std::io::{BufRead, Write};
use std::time::Duration;

/// Which `claude` model the character runs on when `VRCHATCORE_MODEL` does not say. The one the
/// numbers in Docs/DESIGN_ChatCore.md were measured on.
const DEFAULT_MODEL: &str = "haiku";
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
}

/// `serve [--engine echo|claude] [--warmup-ms N]`. Strict on purpose: a flag that is misspelled
/// and ignored would be a self-test that runs the engine it meant to avoid.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options { engine: None, warmup: Duration::ZERO };
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let mut value = |what: &str| args.next().ok_or_else(|| format!("{flag} needs {what}"));
        match flag.as_str() {
            "--engine" => {
                let name = value("echo or claude")?;
                options.engine = Some(Engine::from_name(name).ok_or_else(|| format!("--engine is echo or claude, not {name:?}"))?);
            }
            "--warmup-ms" => {
                let ms = value("a number of milliseconds")?;
                options.warmup = Duration::from_millis(ms.parse().map_err(|_| format!("--warmup-ms needs a whole number, not {ms:?}"))?);
            }
            other => return Err(format!("serve does not take {other:?}; it takes --engine and --warmup-ms")),
        }
    }
    Ok(options)
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

    // Declared before the engine, so it is dropped after it: the directory cannot be removed
    // while the engine's process is still running in it.
    let scratch = Scratch::under(&std::env::temp_dir());
    let config = Config {
        engine: options.engine,
        model: std::env::var(MODEL_ENV).ok().filter(|m| !m.is_empty()).unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        claude: claude::candidates(std::env::var_os(claude::EXE_ENV), std::env::var_os("USERPROFILE")),
        scratch: scratch.root().to_path_buf(),
        warmup: options.warmup,
        turn_timeout: claude::TURN_TIMEOUT,
        speaker: Speaker::new(SpeechEnv::from_process(), &|name| std::env::var_os(name)),
    };

    let mut input = std::io::stdin().lock();
    emit(&mut std::io::stdout(), &Event::Ready { version: env!("CARGO_PKG_VERSION") })?;
    // After `ready` and not before: nothing the engine does, a slow start included, can hold it up.
    // Stdout is not locked for the whole run because the engine writes to it from its own thread.
    let engine = engine::start(config, Box::new(|event| emit(&mut std::io::stdout(), event)));

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
            Ok(Request::Say { text }) => reported(engine.say(text))?,
            Ok(Request::Character(character)) => reported(engine.set_character(character))?,
            Ok(Request::Greet(greeting)) => reported(engine.greet(greeting))?,
            Err(message) => {
                eprintln!("vrchatcore: {message}");
                emit(&mut std::io::stdout(), &Event::Error { message })?;
            }
        }
    }
}
