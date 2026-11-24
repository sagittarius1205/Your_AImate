// The resident `claude` process: how it is started, how a sentence goes in and an answer comes out.
//
// The shape and the flags are the ones measured in Docs/DESIGN_ChatCore.md ("引擎的形狀"). The
// process is started with the neutral directory as its cwd, because the CLI puts the working
// directory's git state and CLAUDE.md into the character's context and neither `--tools ""` nor
// `--restricted` stops it. `--effort` is deliberately not here: how long the model thinks is not
// the cost being attacked, the process start and the carrying are. `--include-partial-messages`
// is: with it the CLI streams the answer's JSON as the model writes it, so the first clause can be
// voiced while the rest is still being written (Docs/DESIGN_ChatCore.md §15).
//
// Nothing in this file decides what an answer means. It hands back the `result` message and
// `answer` turns that into a `Reply` through the checks in `reply.rs`. From the stream it hands
// over the first clause, which `reply::FirstClause` has checked, and nothing else.

use crate::character::Character;
use crate::reply::{self, Clause, FirstClause, Relaying, Reply, Streamed};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
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

/// The variable Claude Desktop sets on what it starts, to say which program started it. A
/// `claude -p` that inherits `claude-desktop` ends every turn with an extra `system/post_turn_summary`
/// step of 2.3 to 2.8 s. Measured 2026-11-13 (claude 2.1.309, haiku): the same core relayed in 9.7 s
/// when it inherited a Claude Desktop session's environment and in 4.2 s from a clean one, and with
/// the whole inherited environment kept and only this variable removed the step was gone. Only this
/// one is removed: the user may have signed `claude` in through other `CLAUDE_CODE_*` or
/// `ANTHROPIC_*` variables, which it needs.
const ENTRYPOINT_ENV: &str = "CLAUDE_CODE_ENTRYPOINT";

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
pub fn command(program: &OsStr, cwd: &Path, model: &str, character: &Character, relaying: Relaying) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
        .arg("--json-schema")
        .arg(reply::schema(relaying).to_string())
        .arg("--append-system-prompt")
        .arg(reply::prompt(character, relaying))
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
    /// The answer as it is being written.
    Stream(Stream),
    /// Every other message. A turn is made of many (assistant text, thinking, rate limits) and
    /// none of them is the answer.
    Other,
    /// Not a message at all. The stream can no longer be trusted to line up with the questions.
    Bad(String),
}

/// The name of the tool `--json-schema` adds, which the model answers by calling (measured: every
/// streamed turn of Docs/MEASURE_TurnLatency.md has one block of it).
pub const ANSWER_TOOL: &str = "StructuredOutput";

/// What `--include-partial-messages` adds that the answer is made of. Nothing here is judged: which
/// pieces belong to which attempt is `await_answer`'s, and what they say is `reply::FirstClause`'s.
#[derive(Debug, PartialEq)]
pub enum Stream {
    /// The model begins a call of `ANSWER_TOOL`, content block `index` of its message: one attempt
    /// at the answer. The CLI asks again when an attempt fails its schema, so a turn can hold more.
    Attempt { index: u64 },
    /// A piece of the JSON input of the tool call in block `index`.
    Piece { index: u64, json: String },
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
        (Some("stream_event"), _) => value.get("event").and_then(stream).map_or(Line::Other, Line::Stream),
        _ => Line::Other,
    }
}

/// The two kinds of stream event the answer is made of. Thinking, text and every other event are
/// not the answer, so they are not handed on.
fn stream(event: &Value) -> Option<Stream> {
    let index = event.get("index").and_then(Value::as_u64)?;
    let text = |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    match event.get("type").and_then(Value::as_str)? {
        "content_block_start" => {
            let block = event.get("content_block")?;
            let answer = text(block, "type").as_deref() == Some("tool_use") && text(block, "name").as_deref() == Some(ANSWER_TOOL);
            answer.then_some(Stream::Attempt { index })
        }
        "content_block_delta" => {
            let delta = event.get("delta")?;
            let input = text(delta, "type").as_deref() == Some("input_json_delta");
            input.then(|| text(delta, "partial_json")).flatten().map(|json| Stream::Piece { index, json })
        }
        _ => None,
    }
}

/// The `result` message as a `Reply`: the first decode, then the checks in `reply::parse`.
pub fn answer(message: &Value, relaying: Relaying) -> Result<Reply, String> {
    reply::parse(result(message)?, relaying)
}

/// The first decode alone: the answer's text out of the `result` message, or why there is none.
/// Apart from `reply::parse`, so a turn can say whether claude failed or the core refused it.
pub fn result(message: &Value) -> Result<&str, String> {
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
        Some(Value::String(text)) => Ok(text),
        Some(other) => Err(format!("claude's result is {}, not a string", reply::kind(other))),
        None => Err("claude's result message has no `result` field".to_string()),
    }
}

/// What came off the pipe, already told apart so the waiting side only decides. The answer and its
/// stream carry the instant the reader read them: the side that waits can be busy voicing the
/// first clause while they queue, and how long the model took is when they came, not when they
/// were looked at.
pub enum Msg {
    Result(Value, Instant),
    Stream(Stream, Instant),
    Bad(String),
    /// A line as it came, from a process started with `Lines::All`.
    Raw(Vec<u8>),
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

/// Where a turn's stream stands on its first clause. Only the first attempt at the answer is read
/// for it, and it is handed over once at most: whatever a later attempt writes waits for `result`
/// like the rest of the answer.
enum FirstAttempt {
    Waiting,
    Reading { index: u64, clause: FirstClause },
    Done,
}

impl FirstAttempt {
    /// The first clause, or why the stream will not give one, the first time either is known.
    fn hear(&mut self, moment: Stream) -> Option<Result<Clause, String>> {
        let surprise = |why: &str| Some(Err(why.to_string()));
        match (std::mem::replace(self, FirstAttempt::Done), moment) {
            (FirstAttempt::Waiting, Stream::Attempt { index }) => {
                *self = FirstAttempt::Reading { index, clause: FirstClause::new() };
                None
            }
            (FirstAttempt::Waiting, Stream::Piece { .. }) => surprise("a piece of input came before the answer's tool call began"),
            (FirstAttempt::Reading { index, clause }, Stream::Piece { index: of, json }) if of == index => match clause.push(&json) {
                Streamed::Open(clause) => {
                    *self = FirstAttempt::Reading { index, clause };
                    None
                }
                Streamed::Closed(clause) => Some(Ok(clause)),
                Streamed::Never(why) => Some(Err(why)),
            },
            (FirstAttempt::Reading { .. }, Stream::Piece { .. }) => surprise("a piece of another content block came inside the answer"),
            (FirstAttempt::Reading { .. }, Stream::Attempt { .. }) => surprise("the model began the answer again before its first clause closed"),
            (FirstAttempt::Done, _) => None,
        }
    }
}

/// Waits for the answer to the question asked, until `deadline`. Looks up every `POLL` to see
/// whether the core is shutting down, so a turn in flight never keeps the core from leaving.
///
/// `first` is called at most once, with the stream's first clause and when the piece that closed it
/// was read, or with why there will be none. It runs on this thread and the reader goes on reading
/// meanwhile, so a `result` that arrives while it runs waits in the queue: one read by the deadline
/// is still the answer, one read after it is not.
pub fn await_answer(msgs: &Receiver<Msg>, deadline: Instant, stop: &AtomicBool, first: &mut dyn FnMut(Result<Clause, String>, Instant)) -> Result<(Value, Instant), Wait> {
    let mut attempt = FirstAttempt::Waiting;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Wait::Stopped);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let msg = if left.is_zero() {
            msgs.try_recv().map_err(|e| match e {
                TryRecvError::Empty => RecvTimeoutError::Timeout,
                TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
            })
        } else {
            msgs.recv_timeout(left.min(POLL))
        };
        match msg {
            Ok(Msg::Result(value, read)) if read <= deadline => return Ok((value, read)),
            Ok(Msg::Result(..)) => return Err(Wait::Timeout),
            // Nothing is voiced past the deadline: the turn has failed by then.
            Ok(Msg::Stream(moment, read)) if !left.is_zero() => {
                if let Some(heard) = attempt.hear(moment) {
                    first(heard, read);
                }
            }
            Ok(Msg::Bad(why)) => return Err(Wait::Bad(why)),
            Ok(Msg::Ended(why)) => return Err(Wait::Ended(why)),
            Ok(Msg::Stream(..) | Msg::Raw(_)) => {}
            Err(RecvTimeoutError::Timeout) if left.is_zero() => return Err(Wait::Timeout),
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

/// What the reader hands over from stdout.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    /// The character's process: only what ends a turn, and what breaks the stream.
    Answers,
    /// The courier's: every line, for the caller to read itself.
    All,
}

/// The character's process.
pub fn start(candidates: &[OsString], cwd: &Path, model: &str, character: &Character, relaying: Relaying, withheld: &[&OsStr]) -> Result<Proc, StartError> {
    spawn(candidates, cwd, withheld, Lines::Answers, |program| command(program, cwd, model, character, relaying))
}

/// Starts the first candidate that exists, with the command `make` builds for it. A program that
/// is not there moves on to the next place; any other failure to start is remembered, and is what
/// is reported if nothing starts. The variables `withheld` names are not passed on: they hold the
/// API keys of the speech and chat services, and `claude` has no use for them. Nor is
/// `ENTRYPOINT_ENV`, whoever the caller is.
pub fn spawn(candidates: &[OsString], cwd: &Path, withheld: &[&OsStr], lines: Lines, make: impl Fn(&OsStr) -> Command) -> Result<Proc, StartError> {
    let mut failed = None;
    for program in candidates {
        let mut cmd = make(program);
        cmd.env_remove(ENTRYPOINT_ENV);
        for name in withheld {
            cmd.env_remove(name);
        }
        match cmd.spawn() {
            Ok(child) => return Ok(Proc::attach(child, cwd, lines)),
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
    fn attach(mut child: Child, cwd: &Path, lines: Lines) -> Proc {
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let (tx, msgs) = mpsc::channel();
        let started = Instant::now();
        let cwd = cwd.to_path_buf();
        thread::spawn(move || match lines {
            Lines::Answers => read_stdout(stdout, tx, started, &cwd),
            Lines::All => forward_stdout(stdout, &tx),
        });
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = Arc::clone(&stderr_tail);
        thread::spawn(move || read_stderr(stderr, &tail));
        Proc { child, stdin, msgs, stderr_tail }
    }

    /// `Some` once the process has exited.
    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Hands over one sentence as a user message.
    pub fn write(&mut self, text: &str) -> Result<(), String> {
        let wrote = match self.stdin.as_mut() {
            Some(stdin) => stdin.write_all(user_line(text).as_bytes()).and_then(|()| stdin.flush()),
            None => Err(std::io::Error::other("its input is closed")),
        };
        wrote.map_err(|e| self.death_note(Some(format!("writing to it failed: {e}"))))
    }

    /// The next line from a process started with `Lines::All`, by `deadline`, unless the core is
    /// shutting down first.
    pub fn next_line(&self, deadline: Instant, stop: &AtomicBool) -> Result<Vec<u8>, Wait> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Wait::Stopped);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Wait::Timeout);
            }
            match self.msgs.recv_timeout(left.min(POLL)) {
                Ok(Msg::Raw(line)) => return Ok(line),
                Ok(Msg::Bad(why)) => return Err(Wait::Bad(why)),
                Ok(Msg::Ended(why)) => return Err(Wait::Ended(why)),
                Ok(Msg::Result(..) | Msg::Stream(..)) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(Wait::Ended(None)),
            }
        }
    }

    /// Hands over one sentence and waits for the `result` message that answers it, `limit` from
    /// now, and returns it with when it was read. `first` is `await_answer`'s.
    pub fn ask(&mut self, text: &str, limit: Duration, stop: &AtomicBool, first: &mut dyn FnMut(Result<Clause, String>, Instant)) -> Result<(Value, Instant), Failure> {
        self.write(text).map_err(Failure::Broken)?;
        match await_answer(&self.msgs, Instant::now() + limit, stop, first) {
            Ok(answered) => Ok(answered),
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
    pub fn death_note(&mut self, detail: Option<String>) -> String {
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

    /// Ends the process now and waits until it is gone. For a caller that is about to decide
    /// something from what the process did, and must not have it do more first.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        let got = input.read_until(b'\n', &mut buf);
        let read = Instant::now();
        let msg = match got {
            Ok(0) => Msg::Ended(None),
            Err(e) => Msg::Ended(Some(e.to_string())),
            Ok(_) => match classify(&buf) {
                Line::Result(value) => Msg::Result(value, read),
                Line::Stream(moment) => Msg::Stream(moment, read),
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

/// Reads stdout to its end and hands over every line as it came.
fn forward_stdout(stdout: ChildStdout, tx: &Sender<Msg>) {
    let mut input = BufReader::new(stdout);
    loop {
        let mut buf = Vec::new();
        let msg = match input.read_until(b'\n', &mut buf) {
            Ok(0) => Msg::Ended(None),
            Err(e) => Msg::Ended(Some(e.to_string())),
            Ok(_) => Msg::Raw(buf),
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
