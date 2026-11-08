// The resident `claude` process: how it is started, how a sentence goes in and an answer comes out.
//
// The shape and the flags are the ones measured in Docs/DESIGN_ChatCore.md ("引擎的形狀"). The
// process is started with the neutral directory as its cwd, because the CLI puts the working
// directory's git state and CLAUDE.md into the character's context and neither `--tools ""` nor
// `--restricted` stops it. `--effort` is deliberately not here: how long the model thinks is not
// the cost being attacked, the process start and the carrying are.
//
// Nothing in this file decides what an answer means. It hands back the `result` message and
// `answer` turns that into a `Reply` through the checks in `reply.rs`.

use crate::character::Character;
use crate::reply::{self, Reply};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Names the `claude` executable outright. When it is set it is the only place looked, so a path
/// that is wrong is "not found" and not a quiet run of some other `claude`.
pub const EXE_ENV: &str = "VRCHATCORE_CLAUDE";

/// How long one turn may take before the process is given up on. The measured turns are 3 to 10
/// seconds. A process that never answers is otherwise a character that never answers and never
/// says why.
pub const TURN_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a waiting turn looks up from the pipe to see whether the core is shutting down.
const POLL: Duration = Duration::from_millis(50);

/// How long a process gets to leave on its own once its input is closed, before it is killed.
const LEAVE_GRACE: Duration = Duration::from_millis(400);

/// The last lines of stderr kept for the error message when the process dies.
const STDERR_KEPT: usize = 6;

/// Where to look for `claude`, best first. PATH first, because that is what the user's own shell
/// uses; the known install location is the fallback for an app started from Explorer, which does
/// not always inherit the same PATH (the shape of `claude_candidates()` in the pet's `main.rs`).
pub fn candidates(explicit: Option<OsString>, home: Option<OsString>) -> Vec<OsString> {
    if let Some(path) = explicit.filter(|p| !p.is_empty()) {
        return vec![path];
    }
    let exe = format!("claude{}", std::env::consts::EXE_SUFFIX);
    let mut found = vec![OsString::from("claude")];
    if let Some(home) = home {
        found.push(Path::new(&home).join(".local").join("bin").join(exe).into_os_string());
    }
    found
}

/// The command that starts the engine, not yet started. Separate from `start` so a test can read
/// what would run (the program, the flags, the directory) without running anything.
pub fn command(program: &OsStr, cwd: &Path, model: &str, character: &Character) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
        .arg("--json-schema")
        .arg(reply::schema().to_string())
        .arg("--append-system-prompt")
        .arg(reply::prompt(character))
        .arg("--model")
        .arg(model)
        // Three flags that together leave the character nothing but this conversation. `--tools ""`
        // takes away the built-in tools and `--restricted` the rest of them, but `--restricted`'s own
        // help says to "add --strict-mcp-config to skip MCP servers too": without it this process
        // inherits every MCP server the user is signed in to, measured as eight of them including
        // Gmail and Google Drive. A desktop pet has no business reaching those, and a tool call the
        // core cannot answer would hang the turn until the timeout. `--no-session-persistence` is
        // because otherwise every process leaves a transcript directory under the user's `.claude`,
        // one per start, that nothing ever removes.
        .args(["--tools", "", "--restricted", "--strict-mcp-config", "--no-session-persistence"])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Without this a GUI parent flashes a console window for the process.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// One sentence as the user message `claude` reads on stdin: one JSON object on one line.
/// `to_string` escapes every newline and quote in `text`, so the framing cannot be broken by what
/// was said.
pub fn user_line(text: &str) -> String {
    let mut line = json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
    })
    .to_string();
    line.push('\n');
    line
}

/// One line of the process's stdout.
#[derive(Debug, PartialEq)]
pub enum Line {
    /// The end of a turn: the message that carries the answer.
    Result(Value),
    /// The process announcing itself. Carries the model and the directory it really runs in.
    Init(Value),
    /// Every other message. A turn is made of many (assistant text, rate limits) and none of them
    /// is the answer.
    Other,
    /// Not a message at all. The stream can no longer be trusted to line up with the questions.
    Bad(String),
}

pub fn classify(line: &[u8]) -> Line {
    let line = line.trim_ascii();
    if line.is_empty() {
        return Line::Other;
    }
    let text = match std::str::from_utf8(line) {
        Ok(text) => text,
        Err(e) => return Line::Bad(format!("a line that is not valid UTF-8 ({e})")),
    };
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(e) => return Line::Bad(format!("a line that is not JSON ({e}): {}", reply::excerpt(text))),
    };
    if !value.is_object() {
        return Line::Bad(format!("JSON that is {}, not a message: {}", reply::kind(&value), reply::excerpt(text)));
    }
    match (value.get("type").and_then(Value::as_str), value.get("subtype").and_then(Value::as_str)) {
        (Some("result"), _) => Line::Result(value),
        (Some("system"), Some("init")) => Line::Init(value),
        _ => Line::Other,
    }
}

/// The `result` message as a `Reply`: the first decode, then the checks in `reply::parse`.
pub fn answer(message: &Value) -> Result<Reply, String> {
    let subtype = message.get("subtype").and_then(Value::as_str);
    let flagged = message.get("is_error").and_then(Value::as_bool).unwrap_or(false);
    if flagged || subtype.is_some_and(|s| s != "success") {
        let said = match message.get("result") {
            Some(Value::String(text)) if !text.is_empty() => text.clone(),
            _ => message.get("errors").map(Value::to_string).unwrap_or_default(),
        };
        let which = subtype.map(|s| format!(" ({s})")).unwrap_or_default();
        return Err(format!("claude reported an error{which}: {}", reply::excerpt(&said)));
    }
    match message.get("result") {
        Some(Value::String(text)) => reply::parse(text),
        Some(other) => Err(format!("claude's result is {}, not a string", reply::kind(other))),
        None => Err("claude's result message has no `result` field".to_string()),
    }
}

/// What came off the pipe, already told apart so the waiting side only decides.
pub enum Msg {
    Result(Value),
    Bad(String),
    /// The pipe closed (`None`) or failed to read (`Some`).
    Ended(Option<String>),
}

#[derive(Debug, PartialEq)]
pub enum Wait {
    Bad(String),
    Ended(Option<String>),
    Timeout,
    Stopped,
}

/// Waits for the answer to the question just asked. Looks up every `POLL` to see whether the core
/// is shutting down, so a turn in flight never keeps the core from leaving.
pub fn await_result(msgs: &Receiver<Msg>, limit: Duration, stop: &AtomicBool) -> Result<Value, Wait> {
    let deadline = Instant::now() + limit;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Wait::Stopped);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Wait::Timeout);
        }
        match msgs.recv_timeout(left.min(POLL)) {
            Ok(Msg::Result(value)) => return Ok(value),
            Ok(Msg::Bad(why)) => return Err(Wait::Bad(why)),
            Ok(Msg::Ended(why)) => return Err(Wait::Ended(why)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(Wait::Ended(None)),
        }
    }
}

pub enum StartError {
    /// Nothing at any of the places looked.
    NotFound(Vec<OsString>),
    Failed(String),
}

impl StartError {
    pub fn describe(&self) -> String {
        match self {
            StartError::NotFound(tried) => {
                let places: Vec<_> = tried.iter().map(|p| p.to_string_lossy()).collect();
                format!("claude was not found (looked for: {})", places.join(", "))
            }
            StartError::Failed(why) => format!("claude could not be started ({why})"),
        }
    }
}

pub enum Failure {
    /// The core is shutting down. Not a fault.
    Stopped,
    /// The process is dead, hung or no longer in step with the questions. The message says which,
    /// and the process is not worth keeping.
    Broken(String),
}

pub struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    msgs: Receiver<Msg>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

/// Starts the first candidate that exists. A program that is not there moves on to the next place;
/// any other failure to start is remembered, and is what is reported if nothing starts. The
/// variable `withheld` names is not passed on: it holds the speech service's API key, and `claude`
/// has no use for it.
pub fn start(candidates: &[OsString], cwd: &Path, model: &str, character: &Character, withheld: Option<&OsStr>) -> Result<Proc, StartError> {
    let mut failed = None;
    for program in candidates {
        let mut cmd = command(program, cwd, model, character);
        if let Some(name) = withheld {
            cmd.env_remove(name);
        }
        match cmd.spawn() {
            Ok(child) => return Ok(Proc::attach(child, cwd)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failed = failed.or_else(|| Some(format!("{}: {e}", program.to_string_lossy()))),
        }
    }
    Err(match failed {
        Some(why) => StartError::Failed(why),
        None => StartError::NotFound(candidates.to_vec()),
    })
}

impl Proc {
    fn attach(mut child: Child, cwd: &Path) -> Proc {
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let (tx, msgs) = mpsc::channel();
        let started = Instant::now();
        let cwd = cwd.to_path_buf();
        thread::spawn(move || read_stdout(stdout, tx, started, &cwd));
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = Arc::clone(&stderr_tail);
        thread::spawn(move || read_stderr(stderr, &tail));
        Proc { child, stdin, msgs, stderr_tail }
    }

    /// `Some` once the process has exited.
    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Hands over one sentence and waits for the `result` message that answers it.
    pub fn ask(&mut self, text: &str, limit: Duration, stop: &AtomicBool) -> Result<Value, Failure> {
        let wrote = match self.stdin.as_mut() {
            Some(stdin) => stdin.write_all(user_line(text).as_bytes()).and_then(|()| stdin.flush()),
            None => Err(std::io::Error::other("its input is closed")),
        };
        if let Err(e) = wrote {
            return Err(Failure::Broken(self.death_note(Some(format!("writing to it failed: {e}")))));
        }
        match await_result(&self.msgs, limit, stop) {
            Ok(message) => Ok(message),
            Err(Wait::Stopped) => Err(Failure::Stopped),
            Err(Wait::Timeout) => {
                Err(Failure::Broken(format!("claude did not answer within {} seconds, so it was stopped", limit.as_secs())))
            }
            Err(Wait::Bad(why)) => Err(Failure::Broken(format!("claude printed {why}"))),
            Err(Wait::Ended(read_error)) => Err(Failure::Broken(self.death_note(read_error))),
        }
    }

    /// Why the process is gone, for the person who just asked it something: the exit code when it
    /// has one, and the last thing it said on stderr.
    fn death_note(&mut self, detail: Option<String>) -> String {
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = self.exited() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                break None;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let mut note = match (status, detail) {
            (Some(status), _) => match status.code() {
                Some(code) => format!("claude exited (code {code}) before it answered"),
                None => "claude was ended by the system before it answered".to_string(),
            },
            (None, Some(detail)) => format!("claude stopped answering ({detail})"),
            (None, None) => "claude closed its output but did not exit".to_string(),
        };
        let tail = self.stderr_tail.lock().map(|t| t.iter().cloned().collect::<Vec<_>>().join(" | ")).unwrap_or_default();
        if !tail.is_empty() {
            note.push_str(&format!(". What it last said on stderr: {tail}"));
        }
        note
    }

    /// Lets the process leave by closing its input, and kills it if it has not gone shortly after.
    /// Waits for it either way, because on Windows a directory cannot be removed while a process
    /// still has it as its working directory.
    pub fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + LEAVE_GRACE;
        while self.exited().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads stdout to its end. An `init` message is logged, with the directory the process says it
/// is running in: that line is the evidence the neutral directory took.
fn read_stdout(stdout: ChildStdout, tx: Sender<Msg>, started: Instant, cwd: &Path) {
    let mut input = BufReader::new(stdout);
    let mut buf = Vec::new();
    let mut announced = false;
    loop {
        buf.clear();
        let msg = match input.read_until(b'\n', &mut buf) {
            Ok(0) => Msg::Ended(None),
            Err(e) => Msg::Ended(Some(e.to_string())),
            Ok(_) => match classify(&buf) {
                Line::Result(value) => Msg::Result(value),
                Line::Bad(why) => Msg::Bad(why),
                Line::Init(init) => {
                    // The CLI announces itself again at the start of every turn. Once is the news.
                    if !announced {
                        announced = true;
                        let field = |key: &str| init.get(key).and_then(Value::as_str).unwrap_or("?").to_string();
                        eprintln!(
                            "vrchatcore: claude is up {} ms after it was started (model {}, cwd {}, asked to run in {})",
                            started.elapsed().as_millis(),
                            field("model"),
                            field("cwd"),
                            cwd.display()
                        );
                    }
                    continue;
                }
                Line::Other => continue,
            },
        };
        let last = matches!(msg, Msg::Ended(_));
        if tx.send(msg).is_err() || last {
            return;
        }
    }
}

/// Reads stderr to its end, for two reasons: a pipe nobody reads fills and stalls the process that
/// writes to it, and the last lines are what explain a death.
fn read_stderr(stderr: ChildStderr, tail: &Mutex<VecDeque<String>>) {
    let mut input = BufReader::new(stderr);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if !matches!(input.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
            return;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        eprintln!("claude: {line}");
        if let Ok(mut kept) = tail.lock() {
            if kept.len() == STDERR_KEPT {
                kept.pop_front();
            }
            kept.push_back(line.chars().take(300).collect());
        }
    }
}
