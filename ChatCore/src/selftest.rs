// The self-test. `checks.rs` holds the cases that need no process. This file holds the ones that
// need a real `serve` on real pipes, which a direct function call would skip: the transport
// (process, pipes, line framing) and the engine's handling of a process.
//
// Nothing here can reach a real `claude`. A core is started in one of three modes and none of them
// leaves the engine to chance: the echo engine, the engine pointed at the stand-in in `stub.rs`, or
// the engine pointed at a place nothing lives. Any call to the real thing is a subscription spent.
//
// Nothing here reaches the real speech service either. Every core is pointed at `FakeSpeech`, a
// listener in this process that answers with a fixed clip, or at an address chosen for the failure
// under test. A turn through the real service is seconds of GPU and a different answer each time.
// The HTTP chat engine is pointed at `FakeChat` the same way, never at a real model.
//
// The transport check deliberately does not use `protocol`: it checks the wire against literal
// JSON, not the structs that wrote it.
//
// Every failure is a named Err that says which assertion broke and what was seen, because a
// self-test that can only say "failed" costs a debugging session.

use crate::character::{Character, DEFAULT_VOICE};
use crate::chat;
use crate::checks;
use crate::claude;
use crate::relay;
use crate::reply;
use crate::scratch;
use crate::serve;
use crate::stub;
use crate::voice;
use base64::prelude::{BASE64_STANDARD, Engine as _};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitCode, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const STEP_TIMEOUT: Duration = Duration::from_secs(5);

/// How the core under test is started.
enum Mode {
    /// The echo engine, optionally held back so a sentence can arrive before the engine is up.
    Echo { warmup_ms: Option<u64> },
    /// The `claude` engine, with `claude` replaced by the stand-in. It appends a line about each
    /// start to `log`. `model` is what the environment says the model is, if it says.
    Stub { log: PathBuf, model: Option<&'static str> },
    /// `claude` is nowhere to be found. With `required` the `claude` engine is asked for by name;
    /// without it nothing says which engine, so the core has to choose.
    Absent { required: bool },
    /// The HTTP engine, sent to `url` and asked for `CHAT_MODEL`.
    Openai { url: String },
    /// `--relay`, with the stand-in as both the character and the courier. The courier does what
    /// `behaviour` says (stub.rs), writes what it ran to `sent` and what it was told to `heard`.
    Courier { log: PathBuf, sent: PathBuf, heard: PathBuf, behaviour: &'static str, step_ms: Option<u64> },
}

/// The model every core in `Mode::Openai` asks for.
const CHAT_MODEL: &str = "selftest-model";

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Result<String, String>>,
    stderr: Option<Receiver<String>>,
    seen: usize,
    /// Every stdout line the test has read, as the core wrote it.
    transcript: Vec<String>,
    log: Option<PathBuf>,
}

impl Client {
    /// `env` is what the core is given besides the address of its speech service. The other speech
    /// variables are taken out first, so a test run from a shell that has them set means the same
    /// thing as one run from a clean one.
    fn spawn(mode: &Mode, speech_url: &str, env: &[(&str, &str)]) -> Result<Client, String> {
        let exe = std::env::current_exe().map_err(|e| format!("locating own executable: {e}"))?;
        let mut cmd = Command::new(&exe);
        cmd.arg("serve").env(voice::URL_ENV, speech_url);
        for name in [voice::MODEL_ENV, voice::VOICE_ENV, voice::KEY_ENV_ENV, chat::ENGINE_ENV, chat::URL_ENV, chat::MODEL_ENV, chat::KEY_ENV_ENV] {
            cmd.env_remove(name);
        }
        cmd.envs(env.iter().copied());
        let mut log = None;
        match mode {
            Mode::Echo { warmup_ms } => {
                cmd.args(["--engine", "echo"]);
                if let Some(ms) = warmup_ms {
                    cmd.args(["--warmup-ms", &ms.to_string()]);
                }
            }
            Mode::Stub { log: path, model } => {
                let _ = std::fs::remove_file(path);
                cmd.args(["--engine", "claude"]).env(claude::EXE_ENV, &exe).env(stub::LOG_ENV, path).env_remove(serve::MODEL_ENV);
                if let Some(model) = model {
                    cmd.env(serve::MODEL_ENV, model);
                }
                log = Some(path.clone());
            }
            Mode::Absent { required } => {
                // The named path is the only place looked. PATH and the home directory are pointed
                // at nothing as well, so a mistake in that rule could not find the real `claude`.
                let nowhere = std::env::temp_dir().join("vrchatcore-selftest-no-such-place");
                cmd.env(claude::EXE_ENV, nowhere.join("claude.exe")).env("PATH", &nowhere).env("USERPROFILE", &nowhere);
                if *required {
                    cmd.args(["--engine", "claude"]);
                }
            }
            Mode::Openai { url } => {
                cmd.args(["--engine", "openai"]).env(chat::URL_ENV, url).env(chat::MODEL_ENV, CHAT_MODEL);
            }
            Mode::Courier { log: path, sent, heard, behaviour, step_ms } => {
                for file in [path, sent, heard] {
                    let _ = std::fs::remove_file(file);
                }
                cmd.args(["--engine", "claude", "--relay"])
                    .env(claude::EXE_ENV, &exe)
                    .env(stub::LOG_ENV, path)
                    .env(stub::SENT_ENV, sent)
                    .env(stub::HEARD_ENV, heard)
                    .env(stub::COURIER_ENV, behaviour)
                    .env_remove(serve::MODEL_ENV);
                if let Some(ms) = step_ms {
                    cmd.args(["--relay-step-ms", &ms.to_string()]);
                }
                log = Some(path.clone());
            }
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawning {} serve: {e}", exe.display()))?;

        let stdin = child.stdin.take().ok_or("child has no stdin pipe")?;
        let stdout = child.stdout.take().ok_or("child has no stdout pipe")?;
        let mut stderr_pipe = child.stderr.take().ok_or("child has no stderr pipe")?;

        // A reader thread, so a child that never answers is a timeout and not a hung test.
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let failed = line.is_err();
                if tx.send(line.map_err(|e| e.to_string())).is_err() || failed {
                    break;
                }
            }
        });
        let (done, stderr) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr_pipe.read_to_string(&mut text);
            let _ = done.send(text);
        });

        Ok(Client { child, stdin: Some(stdin), lines, stderr: Some(stderr), seen: 0, transcript: Vec::new(), log })
    }

    fn send(&mut self, line: &[u8]) -> Result<(), String> {
        let mut framed = line.to_vec();
        framed.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or("stdin was already closed")?;
        stdin
            .write_all(&framed)
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("writing to child stdin (child gone?): {e}"))
    }

    fn recv(&mut self, expecting: &str) -> Result<Value, String> {
        match self.lines.recv_timeout(STEP_TIMEOUT) {
            Ok(Ok(line)) => {
                self.seen += 1;
                let parsed = serde_json::from_str(&line).map_err(|e| format!("stdout line {} is not JSON ({e}): {line:?}", self.seen));
                self.transcript.push(line);
                parsed
            }
            Ok(Err(e)) => Err(format!("reading child stdout while waiting for {expecting}: {e}")),
            Err(RecvTimeoutError::Timeout) => {
                Err(format!("no output within {STEP_TIMEOUT:?} while waiting for {expecting}"))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(format!("child closed stdout while waiting for {expecting}"))
            }
        }
    }

    fn expect(&mut self, what: &str, want: Value) -> Result<(), String> {
        let got = self.recv(what)?;
        if got == want {
            Ok(())
        } else {
            Err(format!("{what}: expected {want}, saw {got}"))
        }
    }

    fn expect_ready(&mut self) -> Result<(), String> {
        self.expect("ready is the first line on stdout", json!({ "ev": "ready", "version": env!("CARGO_PKG_VERSION") }))
    }

    fn say(&mut self, text: &str) -> Result<(), String> {
        self.send(json!({ "op": "say", "text": text }).to_string().as_bytes())
    }

    fn greet(&mut self, text: &str) -> Result<(), String> {
        self.send(json!({ "op": "greet", "text": text }).to_string().as_bytes())
    }

    /// `voice` of `None` leaves the field out, which is the default voice.
    fn character(&mut self, name: &str, persona: &str, voice: Option<&str>) -> Result<(), String> {
        let mut line = json!({ "op": "character", "name": name, "persona": persona });
        if let Some(voice) = voice {
            line["voice"] = json!(voice);
        }
        self.send(line.to_string().as_bytes())
    }

    /// One sentence in, one utterance in the fake service's voice and the end of the turn out.
    fn expect_turn(&mut self, what: &str, text: &str, tags: Value) -> Result<(), String> {
        self.expect_parts(what, &[(text, tags)])
    }

    /// One sentence in, its parts in the fake service's voice, numbered from 0, and the end of the
    /// turn out.
    fn expect_parts(&mut self, what: &str, parts: &[(&str, Value)]) -> Result<(), String> {
        for (part, (text, tags)) in parts.iter().enumerate() {
            self.expect_voiced(what, part, text, tags.clone())?;
        }
        self.expect(&format!("{what}: followed by turn_end"), json!({ "ev": "turn_end" }))
    }

    fn expect_voiced(&mut self, what: &str, part: usize, text: &str, tags: Value) -> Result<(), String> {
        let voice = FakeSpeech::voice();
        self.expect(
            &format!("{what}: part {part}"),
            json!({ "ev": "utterance", "part": part, "text": text, "tags": tags, "voice": voice }),
        )
    }

    /// The utterance with no audio and a reason that holds every needle, and the end of the turn.
    fn expect_silent(&mut self, what: &str, part: usize, text: &str, tags: Value, needles: &[&str]) -> Result<(), String> {
        let got = self.recv(&format!("{what}: an utterance"))?;
        let why = got["voice"]["why"].as_str().unwrap_or_default();
        let shaped = got["ev"] == "utterance"
            && got["part"] == part
            && got["text"] == text
            && got["tags"] == tags
            && got.as_object().is_some_and(|o| o.len() == 5)
            && got["voice"]["state"] == "silent"
            && got["voice"].as_object().is_some_and(|o| o.len() == 2);
        if !shaped {
            return Err(format!(
                "{what}: expected silent part {part} of {text:?} with tags {tags} whose voice is only state and why, saw {got}"
            ));
        }
        if let Some(missing) = needles.iter().find(|n| !why.contains(**n)) {
            return Err(format!("{what}: the reason should say {missing:?}, it says {why:?}"));
        }
        self.expect(&format!("{what}: followed by turn_end"), json!({ "ev": "turn_end" }))
    }

    /// The next line must be an `error` event whose message holds every needle, and nothing else.
    fn expect_error(&mut self, what: &str, needles: &[&str]) -> Result<(), String> {
        let got = self.recv(&format!("{what}: an error event"))?;
        let message = got["message"].as_str().unwrap_or_default();
        let shaped = got["ev"] == "error" && got.as_object().is_some_and(|o| o.len() == 2);
        if !shaped {
            return Err(format!("{what}: expected an error event, saw {got}"));
        }
        match needles.iter().find(|n| !message.contains(**n)) {
            Some(missing) => Err(format!("{what}: the error should say {missing:?}, it says {message:?}")),
            None => Ok(()),
        }
    }

    fn wait_exit(&mut self) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
                Ok(None) => return Err(format!("child still running {STEP_TIMEOUT:?} after shutdown")),
                Err(e) => return Err(format!("waiting for child: {e}")),
            }
        }
    }

    /// Asks the core to leave and checks that it does: exit code 0, and no output after it.
    fn shutdown(&mut self) -> Result<Duration, String> {
        let sent = Instant::now();
        self.send(br#"{"op":"shutdown"}"#)?;
        let status = self.wait_exit()?;
        let took = sent.elapsed();
        if status.code() != Some(0) {
            return Err(format!("shutdown should exit 0, child exited with {status}"));
        }
        match self.lines.recv_timeout(STEP_TIMEOUT) {
            Err(RecvTimeoutError::Disconnected) => Ok(took),
            Ok(Ok(line)) => Err(format!("stdout had output after shutdown: {line:?}")),
            Ok(Err(e)) => Err(format!("reading stdout after shutdown: {e}")),
            Err(RecvTimeoutError::Timeout) => Err("stdout still open after child exit".into()),
        }
    }

    /// Ends the core, politely first so it can clean up after itself, and returns what it wrote on
    /// stderr.
    fn stop(&mut self) -> String {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.send(br#"{"op":"shutdown"}"#);
            let deadline = Instant::now() + Duration::from_millis(1500);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        // Bounded: a process the core left running holds the pipe open, and a test that hangs on
        // that instead of failing is worse than one that is missing its last lines of stderr.
        self.stderr.take().and_then(|text| text.recv_timeout(Duration::from_secs(1)).ok()).unwrap_or_default()
    }

    /// Ends the core and checks that nothing it wrote, on stdout so far or on stderr in all, holds
    /// `secret`. Call it after the last turn: stderr is only whole once the core is gone.
    fn never_said(&mut self, secret: &str) -> Result<(), String> {
        if let Some(line) = self.transcript.iter().find(|l| l.contains(secret)) {
            return Err(format!("the core wrote the secret on stdout: {line}"));
        }
        let stderr = self.stop();
        match stderr.lines().find(|l| l.contains(secret)) {
            Some(line) => Err(format!("the core wrote the secret on stderr: {line}")),
            None => Ok(()),
        }
    }

    /// The error with what the core said on stderr added, which is most of a diagnosis.
    fn explain(&mut self, error: String) -> String {
        let stderr = self.stop();
        if stderr.trim().is_empty() { error } else { format!("{error} (child stderr: {})", stderr.trim()) }
    }

    /// One line per start of the stand-in, as it wrote them.
    fn starts(&self) -> Vec<Value> {
        let text = self.log.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Waits until the stand-in has started `n` times, and returns what it wrote about the last.
    fn wait_for_start(&self, n: usize) -> Result<Value, String> {
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            let starts = self.starts();
            if let Some(last) = starts.get(n.wrapping_sub(1)).filter(|_| starts.len() >= n) {
                return Ok(last.clone());
            }
            if Instant::now() >= deadline {
                return Err(format!("the stand-in had started {} times {STEP_TIMEOUT:?} after the core did, wanted {n}", starts.len()));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
        if let Some(log) = &self.log {
            let _ = std::fs::remove_file(log);
        }
    }
}

/// Starts a core in `mode` with its voice from `FakeSpeech`, runs `body` on it, and in every case
/// leaves no process behind.
fn with_core<T>(mode: Mode, body: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
    with_core_speaking_to(mode, &FakeSpeech::get()?.url, body)
}

fn with_core_speaking_to<T>(mode: Mode, speech_url: &str, body: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
    with_core_configured(mode, speech_url, &[], body)
}

fn with_core_configured<T>(mode: Mode, speech_url: &str, env: &[(&str, &str)], body: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
    let mut client = Client::spawn(&mode, speech_url, env)?;
    body(&mut client).map_err(|e| client.explain(e))
}

fn with_stub<T>(name: &str, body: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
    with_stub_model(name, None, body)
}

fn with_stub_model<T>(name: &str, model: Option<&'static str>, body: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
    with_stub_at(name, model, &FakeSpeech::get()?.url, body)
}

fn with_stub_at<T>(name: &str, model: Option<&'static str>, speech_url: &str, body: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
    let log = std::env::temp_dir().join(format!("vrchatcore-selftest-{}-{name}.log", std::process::id()));
    with_core_speaking_to(Mode::Stub { log, model }, speech_url, |client| {
        client.expect_ready()?;
        body(client)
    })
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let listing = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"]).output();
        listing.map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\""))).unwrap_or(true)
    }
    #[cfg(not(windows))]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

fn wait_gone(pid: u32) -> Result<(), String> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    while alive(pid) {
        if Instant::now() >= deadline {
            return Err(format!("process {pid} was still running {STEP_TIMEOUT:?} later"));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn pid_of(start: &Value) -> Result<u32, String> {
    start["pid"].as_u64().map(|p| p as u32).ok_or_else(|| format!("the stand-in's log line has no pid: {start}"))
}

fn cwd_of(start: &Value) -> Result<PathBuf, String> {
    start["cwd"].as_str().map(PathBuf::from).ok_or_else(|| format!("the stand-in's log line has no cwd: {start}"))
}

type Scenario = (&'static str, fn() -> Result<String, String>);

const SCENARIOS: &[Scenario] = &[
    ("transport_with_the_echo_engine", transport_with_the_echo_engine),
    ("ready_does_not_wait_for_the_engine_and_an_early_say_is_queued", ready_does_not_wait_for_the_engine_and_an_early_say_is_queued),
    ("without_claude_the_echo_engine_runs_and_says_why", without_claude_the_echo_engine_runs_and_says_why),
    ("a_claude_that_cannot_be_started_is_named_on_every_say", a_claude_that_cannot_be_started_is_named_on_every_say),
    ("claude_is_started_with_the_core_and_not_with_the_first_say", claude_is_started_with_the_core_and_not_with_the_first_say),
    ("claude_runs_in_an_empty_directory_of_the_cores_own_making", claude_runs_in_an_empty_directory_of_the_cores_own_making),
    ("claude_is_started_with_the_measured_flags_intact", claude_is_started_with_the_measured_flags_intact),
    ("the_model_can_be_chosen_by_environment", the_model_can_be_chosen_by_environment),
    ("a_claude_answer_reaches_the_wire_with_its_gestures_as_tags", a_claude_answer_reaches_the_wire_with_its_gestures_as_tags),
    ("a_bracket_in_a_claude_clause_rejects_the_turn", a_bracket_in_a_claude_clause_rejects_the_turn),
    ("a_gesture_outside_the_vocabulary_rejects_the_turn", a_gesture_outside_the_vocabulary_rejects_the_turn),
    ("an_undecodable_claude_result_is_an_error_and_the_process_stays", an_undecodable_claude_result_is_an_error_and_the_process_stays),
    ("a_claude_that_dies_is_named_and_replaced", a_claude_that_dies_is_named_and_replaced),
    ("a_claude_that_prints_garbage_is_named_and_replaced", a_claude_that_prints_garbage_is_named_and_replaced),
    ("a_claude_that_left_while_idle_is_replaced_without_an_error", a_claude_that_left_while_idle_is_replaced_without_an_error),
    ("a_noisy_stderr_does_not_stall_a_turn", a_noisy_stderr_does_not_stall_a_turn),
    ("claude_and_its_directory_are_gone_after_shutdown", claude_and_its_directory_are_gone_after_shutdown),
    ("a_turn_in_flight_does_not_keep_the_core_from_leaving", a_turn_in_flight_does_not_keep_the_core_from_leaving),
    ("a_turn_is_voiced_with_the_words_the_bubble_shows", a_turn_is_voiced_with_the_words_the_bubble_shows),
    ("with_no_speech_service_the_words_arrive_and_say_how_to_start_it", with_no_speech_service_the_words_arrive_and_say_how_to_start_it),
    ("a_speech_service_that_answers_an_error_is_named", a_speech_service_that_answers_an_error_is_named),
    ("audio_in_another_format_is_named_and_not_played", audio_in_another_format_is_named_and_not_played),
    ("a_clip_with_no_samples_is_silent_and_not_voiced", a_clip_with_no_samples_is_silent_and_not_voiced),
    ("a_speech_url_off_this_machine_is_refused_by_name", a_speech_url_off_this_machine_is_refused_by_name),
    ("a_clip_at_another_rate_is_converted_and_played", a_clip_at_another_rate_is_converted_and_played),
    ("a_custom_speech_service_gets_its_model_voice_and_key", a_custom_speech_service_gets_its_model_voice_and_key),
    ("without_the_key_variable_every_turn_is_silent_and_says_which", without_the_key_variable_every_turn_is_silent_and_says_which),
    ("a_refusal_that_quotes_the_key_is_shown_without_it", a_refusal_that_quotes_the_key_is_shown_without_it),
    ("a_plain_http_service_elsewhere_is_refused_and_the_key_stays_home", a_plain_http_service_elsewhere_is_refused_and_the_key_stays_home),
    ("an_https_service_that_is_not_there_is_named", an_https_service_that_is_not_there_is_named),
    ("claude_is_started_without_the_speech_key", claude_is_started_without_the_speech_key),
    ("a_long_answer_arrives_in_parts_each_in_its_own_voice", a_long_answer_arrives_in_parts_each_in_its_own_voice),
    ("a_long_answer_with_no_voice_arrives_whole_and_silent", a_long_answer_with_no_voice_arrives_whole_and_silent),
    ("a_part_the_service_refuses_takes_the_rest_of_the_answer_silent", a_part_the_service_refuses_takes_the_rest_of_the_answer_silent),
    ("what_one_turn_learns_about_the_service_paces_the_next", what_one_turn_learns_about_the_service_paces_the_next),
    ("a_new_prompt_restarts_claude_and_a_new_voice_alone_does_not", a_new_prompt_restarts_claude_and_a_new_voice_alone_does_not),
    ("a_character_changes_the_voice_the_service_is_asked_for", a_character_changes_the_voice_the_service_is_asked_for),
    ("a_character_that_breaks_a_limit_is_an_error_and_changes_nothing", a_character_that_breaks_a_limit_is_an_error_and_changes_nothing),
    ("a_greeting_is_voiced_in_the_current_voice_without_claude", a_greeting_is_voiced_in_the_current_voice_without_claude),
    ("a_greeting_with_no_speech_service_is_silent_and_still_ends_the_turn", a_greeting_with_no_speech_service_is_silent_and_still_ends_the_turn),
    ("a_bad_greeting_is_an_error_and_says_nothing", a_bad_greeting_is_an_error_and_says_nothing),
    ("a_character_and_a_greeting_wait_for_the_turn_in_flight", a_character_and_a_greeting_wait_for_the_turn_in_flight),
    ("an_openai_answer_reaches_the_wire_with_its_gestures_as_tags", an_openai_answer_reaches_the_wire_with_its_gestures_as_tags),
    ("an_openai_answer_the_core_refuses_is_named_and_not_remembered", an_openai_answer_the_core_refuses_is_named_and_not_remembered),
    ("an_openai_service_that_is_not_running_is_named", an_openai_service_that_is_not_running_is_named),
    ("the_history_sent_is_the_last_eight_turns", the_history_sent_is_the_last_eight_turns),
    ("a_new_character_starts_the_history_over", a_new_character_starts_the_history_over),
    ("the_chat_key_is_a_header_and_never_a_word_anyone_reads", the_chat_key_is_a_header_and_never_a_word_anyone_reads),
    ("claude_is_started_without_the_chat_key", claude_is_started_without_the_chat_key),
    ("a_chat_turn_ends_at_its_limit_or_at_a_stop", a_chat_turn_ends_at_its_limit_or_at_a_stop),
    ("a_chat_turn_in_flight_does_not_keep_the_core_from_leaving", a_chat_turn_in_flight_does_not_keep_the_core_from_leaving),
    ("the_models_are_listed_for_the_settings_window", the_models_are_listed_for_the_settings_window),
    ("a_faithful_courier_delivers_once", a_faithful_courier_delivers_once),
    ("a_courier_that_rewrites_the_text_sends_nothing", a_courier_that_rewrites_the_text_sends_nothing),
    ("a_message_that_says_send_it_elsewhere_goes_nowhere_else", a_message_that_says_send_it_elsewhere_goes_nowhere_else),
    ("a_courier_that_calls_twice_sends_once", a_courier_that_calls_twice_sends_once),
    ("a_courier_whose_stop_hook_does_not_run_still_delivers", a_courier_whose_stop_hook_does_not_run_still_delivers),
    ("a_courier_that_reaches_for_another_tool_gets_none", a_courier_that_reaches_for_another_tool_gets_none),
    ("a_title_that_forges_a_line_reaches_nobody", a_title_that_forges_a_line_reaches_nobody),
    ("a_session_that_is_gone_is_not_found", a_session_that_is_gone_is_not_found),
    ("two_sessions_with_one_title_are_ambiguous", two_sessions_with_one_title_are_ambiguous),
    ("a_gate_that_does_not_run_stops_before_anything_can_be_sent", a_gate_that_does_not_run_stops_before_anything_can_be_sent),
    ("a_courier_that_dies_or_hangs_is_judged_by_where_it_stopped", a_courier_that_dies_or_hangs_is_judged_by_where_it_stopped),
    ("the_same_order_twice_is_delivered_once", the_same_order_twice_is_delivered_once),
    ("an_order_while_one_runs_is_busy", an_order_while_one_runs_is_busy),
    ("a_relay_ask_turn_has_no_utterance", a_relay_ask_turn_has_no_utterance),
    ("relaying_is_off_unless_serve_is_told", relaying_is_off_unless_serve_is_told),
    ("an_order_that_breaks_a_limit_is_still_answered", an_order_that_breaks_a_limit_is_still_answered),
    ("the_next_sentence_carries_the_last_outcome_once", the_next_sentence_carries_the_last_outcome_once),
    ("an_outcome_outlives_a_sentence_that_was_not_answered", an_outcome_outlives_a_sentence_that_was_not_answered),
    ("the_courier_runs_without_the_speech_and_chat_keys", the_courier_runs_without_the_speech_and_chat_keys),
    ("the_courier_is_started_with_the_pinned_flags_intact", the_courier_is_started_with_the_pinned_flags_intact),
    ("the_courier_and_its_folder_are_gone_after_shutdown", the_courier_and_its_folder_are_gone_after_shutdown),
    ("she_answers_while_a_relay_runs", she_answers_while_a_relay_runs),
    ("a_send_the_gate_never_saw_is_reported_as_it_was", a_send_the_gate_never_saw_is_reported_as_it_was),
    ("the_gate_process_lets_one_of_four_racing_sends_through", the_gate_process_lets_one_of_four_racing_sends_through),
    ("the_stop_hook_process_prints_the_stop_line", the_stop_hook_process_prints_the_stop_line),
    ("the_stand_in_courier_ends_a_turn_where_its_stop_hook_says", the_stand_in_courier_ends_a_turn_where_its_stop_hook_says),
    ("claude_children_do_not_inherit_the_hosts_entrypoint", claude_children_do_not_inherit_the_hosts_entrypoint),
    ("the_outcome_goes_out_before_the_courier_and_its_folder_are_gone", the_outcome_goes_out_before_the_courier_and_its_folder_are_gone),
    ("a_courier_that_times_out_cannot_send_after_the_outcome", a_courier_that_times_out_cannot_send_after_the_outcome),
    ("a_turn_is_kept_only_when_its_say_asks_and_only_where_it_says", a_turn_is_kept_only_when_its_say_asks_and_only_where_it_says),
    ("a_record_that_cannot_be_written_costs_the_turn_nothing", a_record_that_cannot_be_written_costs_the_turn_nothing),
    ("a_relay_turn_is_kept_without_what_she_asked_to_pass_on", a_relay_turn_is_kept_without_what_she_asked_to_pass_on),
    ("a_turn_the_core_is_stopped_in_is_kept_as_interrupted", a_turn_the_core_is_stopped_in_is_kept_as_interrupted),
];

fn transport_with_the_echo_engine() -> Result<String, String> {
    let started = Instant::now();
    with_core(Mode::Echo { warmup_ms: None }, |client| {
        client.expect_ready()?;
        let ready = started.elapsed();

        // Escaped newline and quotes inside the text: the framing must be per line, not per payload.
        let sent = Instant::now();
        client.say("你好 \"world\"\nsecond\tline")?;
        client.expect_turn("first say", "你好 \"world\"\nsecond\tline", json!([]))?;
        let say_time = sent.elapsed();

        let malformed: [(&str, &[u8]); 3] = [
            ("truncated JSON", br#"{"op":"say","text":"#),
            ("unknown op", br#"{"op":"dance"}"#),
            ("invalid UTF-8", b"{\"op\":\"say\",\"text\":\"\xff\"}"),
        ];
        let mut error_time = Duration::ZERO;
        for (name, line) in malformed {
            let sent = Instant::now();
            client.send(line)?;
            let got = client.recv(&format!("malformed line ({name}) yields an error event"))?;
            error_time = error_time.max(sent.elapsed());
            let is_error = got["ev"] == "error"
                && got["message"].as_str().is_some_and(|m| !m.is_empty())
                && got.as_object().is_some_and(|o| o.len() == 2);
            if !is_error {
                return Err(format!("malformed line ({name}) should yield an error event, saw {got}"));
            }
            match client.child.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => {
                    return Err(format!("child exited ({status}) after malformed line ({name})"));
                }
                Err(e) => return Err(format!("checking child after malformed line ({name}): {e}")),
            }
            client.say("still here")?;
            client.expect_turn(&format!("say after malformed line ({name})"), "still here", json!([]))?;
        }

        let exit_time = client.shutdown()?;
        Ok(format!(
            "ready={:.1}ms say={:.1}ms malformed_to_error<={:.1}ms shutdown_to_exit={:.1}ms stdout_lines={}",
            ms(ready),
            ms(say_time),
            ms(error_time),
            ms(exit_time),
            client.seen,
        ))
    })
}

fn ready_does_not_wait_for_the_engine_and_an_early_say_is_queued() -> Result<String, String> {
    const WARMUP_MS: u64 = 600;
    let started = Instant::now();
    with_core(Mode::Echo { warmup_ms: Some(WARMUP_MS) }, |client| {
        client.expect_ready()?;
        let ready = started.elapsed();
        if ready >= Duration::from_millis(WARMUP_MS / 2) {
            return Err(format!("ready took {:.0}ms with an engine that needs {WARMUP_MS}ms, so it waited for the engine", ms(ready)));
        }

        // Both sentences are sent while the engine is still coming up.
        client.say("first, too early")?;
        client.say("second, too early")?;
        client.expect_turn("a say before the engine is up", "first, too early", json!([]))?;
        let first = started.elapsed();
        if first < Duration::from_millis(WARMUP_MS * 9 / 10) {
            return Err(format!("the first say was answered after {:.0}ms, before the engine could have been up ({WARMUP_MS}ms)", ms(first)));
        }
        client.expect_turn("a second queued say, in order", "second, too early", json!([]))?;

        let late = Instant::now();
        client.say("third, after")?;
        client.expect_turn("a say once the engine is up", "third, after", json!([]))?;
        client.shutdown()?;
        Ok(format!("queue: ready={:.1}ms first_answer={:.0}ms (engine start held {WARMUP_MS}ms) answer_when_warm={:.1}ms", ms(ready), ms(first), ms(late.elapsed())))
    })
}

fn without_claude_the_echo_engine_runs_and_says_why() -> Result<String, String> {
    with_core(Mode::Absent { required: false }, |client| {
        client.expect_ready()?;
        client.say("hello?")?;
        client.expect_turn("the echo engine answers", "hello?", json!([]))?;
        client.expect_error("and the character says why it only echoes", &["claude was not found", "claude.exe", "VRCHATCORE_CLAUDE"])?;
        client.say("hello again?")?;
        // The reason is given once. The next line after this turn is the shutdown's silence.
        client.expect_turn("the second sentence is echoed without a second notice", "hello again?", json!([]))?;
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_claude_that_cannot_be_started_is_named_on_every_say() -> Result<String, String> {
    with_core(Mode::Absent { required: true }, |client| {
        client.expect_ready()?;
        // Asked for by name, so there is no falling back to the echo engine: the answer is the
        // reason, every time, and the core stays up to say it.
        for turn in 1..=2 {
            client.say("anyone there?")?;
            client.expect_error(&format!("sentence {turn} with no claude to start"), &["claude was not found", "claude.exe"])?;
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn claude_is_started_with_the_core_and_not_with_the_first_say() -> Result<String, String> {
    let started = Instant::now();
    with_stub("early", |client| {
        // No sentence has been sent. If the stand-in has started, the core started it on its own.
        let start = client.wait_for_start(1)?;
        let up = started.elapsed();
        pid_of(&start)?;
        client.shutdown()?;
        Ok(format!("claude_started_before_any_say={:.0}ms", ms(up)))
    })
}

fn claude_runs_in_an_empty_directory_of_the_cores_own_making() -> Result<String, String> {
    with_stub("cwd", |client| {
        let start = client.wait_for_start(1)?;
        let cwd = cwd_of(&start)?;
        if start["entries"] != json!(0) {
            return Err(format!("the engine's directory should start empty, it held {} entries", start["entries"]));
        }
        let real = std::fs::canonicalize(&cwd).map_err(|e| format!("the engine's directory {} is not there while it runs: {e}", cwd.display()))?;
        let temp = std::fs::canonicalize(std::env::temp_dir()).map_err(|e| format!("resolving the temp directory: {e}"))?;
        if !real.starts_with(&temp) {
            return Err(format!("the engine's directory {} is not under the temp directory {}", real.display(), temp.display()));
        }
        let owner = real.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !owner.starts_with("vrchatcore-") {
            return Err(format!("the engine's directory {} is not inside the core's own temp directory (vrchatcore-<pid>)", real.display()));
        }
        // The repo the self-test runs from is the directory the core inherits. It must not be the engine's.
        let inherited = std::env::current_dir().and_then(std::fs::canonicalize).map_err(|e| format!("resolving the working directory: {e}"))?;
        if real.starts_with(&inherited) || inherited.starts_with(&real) {
            return Err(format!("the engine runs in {}, which is the core's own working directory {}", real.display(), inherited.display()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn claude_is_started_with_the_measured_flags_intact() -> Result<String, String> {
    with_stub("flags", |client| {
        let start = client.wait_for_start(1)?;
        let args: Vec<String> = start["args"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .ok_or_else(|| format!("no args in the stand-in's log line: {start}"))?;
        let after = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1));
        let want = |flag: &str, value: &str| match after(flag) {
            Some(got) if got == value => Ok(()),
            got => Err(format!("{flag} should be followed by {value:?}, it is followed by {got:?}")),
        };
        if args.first().map(String::as_str) != Some("-p") {
            return Err(format!("the first argument should be -p, args are {args:?}"));
        }
        want("--input-format", "stream-json")?;
        want("--output-format", "stream-json")?;
        want("--model", "haiku")?;
        want("--tools", "")?;
        let tail: Vec<&str> = args.iter().rev().take(checks::ISOLATION.len()).rev().map(String::as_str).collect();
        if tail != checks::ISOLATION {
            return Err(format!("the arguments should end with {:?}, they end with {tail:?}", checks::ISOLATION));
        }
        if !args.iter().any(|a| a == "--verbose") {
            return Err("--verbose is missing".to_string());
        }
        if args.iter().any(|a| a == "--effort") {
            return Err("--effort is there, and how long the model thinks is not what is being changed".to_string());
        }
        // The two long values cross a Windows command line. Quotes and CJK must come out the other end intact.
        let schema: Option<Value> = after("--json-schema").and_then(|s| serde_json::from_str(s).ok());
        if schema != Some(reply::schema(reply::Relaying::Off)) {
            return Err(format!("--json-schema arrived as {:?}, not as the schema", after("--json-schema")));
        }
        if after("--append-system-prompt") != Some(&reply::prompt(&Character::standard(), reply::Relaying::Off)) {
            return Err("--append-system-prompt did not arrive as the default character's prompt".to_string());
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn the_model_can_be_chosen_by_environment() -> Result<String, String> {
    with_stub_model("model", Some("a-model-the-environment-named"), |client| {
        let start = client.wait_for_start(1)?;
        let args = start["args"].as_array().cloned().unwrap_or_default();
        let at = args.iter().position(|a| a == "--model").ok_or("no --model in the stand-in's arguments")?;
        if args.get(at + 1) != Some(&json!("a-model-the-environment-named")) {
            return Err(format!("--model should be followed by the model the environment named, saw {:?}", args.get(at + 1)));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_claude_answer_reaches_the_wire_with_its_gestures_as_tags() -> Result<String, String> {
    with_stub("wire", |client| {
        // Sent at once, with no wait for the stand-in: a sentence that beats the process is not lost.
        client.say("ok")?;
        client.expect_turn("a two-clause answer", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        client.say("many")?;
        // Four one-syllable atoms, the last two one clause cut after its comma. Three play 0.63 s and
        // the fourth takes 0.38 s, so the pace sends the fourth as a part of its own; each part lists
        // only its own tags, and a part that starts inside a clause has none.
        client.expect_parts(
            "three clauses, a gesture used twice and one with none",
            &[("[nod]一 二 [nod]三，", json!(["nod"])), ("四", json!([]))],
        )?;
        // A sentence goes through two JSON layers each way. The framing must survive what it holds.
        let awkward = "first line\nsecond \"quoted\" line \\ backslash\ttab and 日本語 😀";
        client.say(&format!("echo:{awkward}"))?;
        client.expect_turn("a sentence full of characters that need escaping", awkward, json!([]))?;
        client.shutdown()?;
        if client.starts().len() != 1 {
            return Err(format!("the stand-in should have been started once, it was started {} times", client.starts().len()));
        }
        Ok(String::new())
    })
}

/// A word the stand-in turns into a bad answer. The turn is an error that says why, the core goes
/// on, and the process is kept: nothing was wrong with the pipe.
fn rejected_and_carried_on(client: &mut Client, word: &str, needles: &[&str]) -> Result<(), String> {
    client.say(word)?;
    client.expect_error(&format!("the stand-in answers {word:?}"), needles)?;
    client.say("ok")?;
    client.expect_turn(&format!("a sentence after {word:?}"), "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))
}

fn a_bracket_in_a_claude_clause_rejects_the_turn() -> Result<String, String> {
    with_stub("bracket", |client| {
        rejected_and_carried_on(client, "bracket", &["clauses[1].text contains", "'['", "[happy] hi"])?;
        if client.starts().len() != 1 {
            return Err("a rejected answer should not cost the process".to_string());
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_gesture_outside_the_vocabulary_rejects_the_turn() -> Result<String, String> {
    with_stub("gesture", |client| {
        rejected_and_carried_on(client, "badgesture", &["clauses[1].gesture", "\"happy\"", "outside the vocabulary", "lookAway"])?;
        if client.starts().len() != 1 {
            return Err("a rejected answer should not cost the process".to_string());
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn an_undecodable_claude_result_is_an_error_and_the_process_stays() -> Result<String, String> {
    with_stub("undecodable", |client| {
        rejected_and_carried_on(client, "empty", &["empty `clauses`"])?;
        rejected_and_carried_on(client, "four", &["4 clauses", "at most 3"])?;
        rejected_and_carried_on(client, "notjson", &["not JSON", "this is not json"])?;
        rejected_and_carried_on(client, "array", &["JSON but not an object", "an array"])?;
        rejected_and_carried_on(client, "error", &["claude reported an error", "error_during_execution", "the stub failed on purpose"])?;
        if client.starts().len() != 1 {
            return Err("a result that cannot be decoded should not cost the process".to_string());
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_claude_that_dies_is_named_and_replaced() -> Result<String, String> {
    with_stub("dies", |client| {
        client.say("die")?;
        client.expect_error("the stand-in exits instead of answering", &["claude exited (code 3)", "boom: the stub died on purpose"])?;
        client.say("ok")?;
        client.expect_turn("the sentence after the death", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        let starts = client.starts();
        if starts.len() != 2 {
            return Err(format!("a dead process should be replaced by exactly one new one, it was started {} times", starts.len()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_claude_that_prints_garbage_is_named_and_replaced() -> Result<String, String> {
    with_stub("garbage", |client| {
        client.say("garbage")?;
        client.expect_error("the stand-in prints something that is not a message", &["claude printed", "not JSON", "this is not json"])?;
        client.say("ok")?;
        client.expect_turn("the sentence after the garbage", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        let starts = client.starts();
        if starts.len() != 2 {
            return Err(format!("a process that cannot be trusted should be replaced by exactly one new one, it was started {} times", starts.len()));
        }
        // The one that printed garbage is not left running.
        wait_gone(pid_of(&starts[0])?)?;
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_claude_that_left_while_idle_is_replaced_without_an_error() -> Result<String, String> {
    with_stub("idle", |client| {
        client.say("bye")?;
        client.expect_turn("the stand-in answers and leaves", "[nod]再見", json!(["nod"]))?;
        let first = client.wait_for_start(1)?;
        wait_gone(pid_of(&first)?)?;
        client.say("ok")?;
        // The very next line is the answer. A replacement that cost this sentence an error would be here.
        client.expect_turn("the sentence after the process left", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        if client.starts().len() != 2 {
            return Err(format!("the process that left should be replaced once, it was started {} times", client.starts().len()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_noisy_stderr_does_not_stall_a_turn() -> Result<String, String> {
    with_stub("noise", |client| {
        let sent = Instant::now();
        client.say("noise")?;
        client.expect_turn("a turn with 300 KB on stderr and forty other messages on stdout", "吵歸吵", json!([]))?;
        let took = sent.elapsed();
        client.shutdown()?;
        Ok(format!("noisy_turn={:.0}ms", ms(took)))
    })
}

fn claude_and_its_directory_are_gone_after_shutdown() -> Result<String, String> {
    with_stub("cleanup", |client| {
        let start = client.wait_for_start(1)?;
        let (pid, cwd) = (pid_of(&start)?, cwd_of(&start)?);
        client.say("ok")?;
        client.expect_turn("a turn while it runs", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        if !cwd.is_dir() {
            return Err(format!("the engine's directory {} should exist while the engine runs", cwd.display()));
        }
        client.shutdown()?;
        wait_gone(pid).map_err(|e| format!("the engine outlived the core: {e}"))?;
        let root = cwd.parent().map(Path::to_path_buf).unwrap_or_default();
        if cwd.exists() || root.exists() {
            return Err(format!("the core's temp directory {} was left behind after shutdown", root.display()));
        }
        Ok(String::new())
    })
}

/// The pet closing while the character is mid-sentence. The process is asked nothing more, does not
/// leave when its input closes, and has to be killed; the directory it was running in has to go
/// after it.
fn a_turn_in_flight_does_not_keep_the_core_from_leaving() -> Result<String, String> {
    with_stub("hang", |client| {
        let start = client.wait_for_start(1)?;
        let (pid, cwd) = (pid_of(&start)?, cwd_of(&start)?);
        client.say("hang")?;
        // Long enough for the core to be waiting on the stand-in.
        thread::sleep(Duration::from_millis(300));
        let took = client.shutdown()?;
        wait_gone(pid).map_err(|e| format!("the engine outlived the core: {e}"))?;
        let root = cwd.parent().map(Path::to_path_buf).unwrap_or_default();
        if root.exists() {
            return Err(format!("the core's temp directory {} was left behind", root.display()));
        }
        Ok(format!("leave_mid_turn={:.0}ms", ms(took)))
    })
}

/// What the stand-in speech service answers with.
#[derive(Clone)]
enum Answer {
    Wav(Vec<u8>),
    Status(u16),
    /// The clip, except a 503 for a request whose input is exactly this.
    RefusingInput(String),
    /// The clip, after this long.
    Late(Duration),
    /// A 401 whose message quotes the bearer token it was given, as some services do.
    QuotingTheKey,
}

/// The speech service, played by a listener in this process. One for the whole run: the scenarios
/// run one after another, and each sets the answer it needs and puts the clip back after.
struct FakeSpeech {
    url: String,
    answer: Mutex<Answer>,
    /// The request line and the JSON body of every request, in order.
    heard: Mutex<Vec<(String, Value)>>,
    /// The `Authorization` header of each of those requests, in the same order.
    authorized: Mutex<Vec<Option<String>>>,
}

/// Three frames, silent, loud and half as loud, and ten samples that make no frame: the envelope a
/// person would write down for it is [0, 1, 0.5].
fn fake_pcm() -> Vec<i16> {
    [vec![0; voice::FRAME_SAMPLES], vec![1000; voice::FRAME_SAMPLES], vec![500; voice::FRAME_SAMPLES], vec![500; 10]].concat()
}

fn pcm_bytes(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// 970 samples, 61 ms of audio: less than the 0.2 s every piece is estimated to cost, so behind a
/// part in this voice no run of a later part ever fits, and each later part is one atom.
fn fake_clip() -> Vec<u8> {
    checks::wav(1, 1, 16000, 16, &pcm_bytes(&fake_pcm()), b"")
}

impl FakeSpeech {
    fn get() -> Result<&'static FakeSpeech, String> {
        static FAKE: OnceLock<FakeSpeech> = OnceLock::new();
        if let Some(fake) = FAKE.get() {
            return Ok(fake);
        }
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("starting the stand-in speech service: {e}"))?;
        let port = listener.local_addr().map_err(|e| format!("its address: {e}"))?.port();
        let fake = FAKE.get_or_init(|| FakeSpeech {
            url: format!("http://127.0.0.1:{port}/v1"),
            answer: Mutex::new(Answer::Wav(fake_clip())),
            heard: Mutex::new(Vec::new()),
            authorized: Mutex::new(Vec::new()),
        });
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                fake.answer_one(stream);
            }
        });
        Ok(fake)
    }

    /// The `voice` every turn carries while the fake answers with its clip.
    fn voice() -> Value {
        json!({ "state": "voiced", "samples": 970, "pcm_b64": BASE64_STANDARD.encode(pcm_bytes(&fake_pcm())), "envelope": [0.0, 1.0, 0.5] })
    }

    fn answer_one(&self, stream: TcpStream) {
        let mut reader = BufReader::new(&stream);
        let mut request_line = String::new();
        let mut length = 0;
        let mut authorization = None;
        let mut line = String::new();
        let _ = reader.read_line(&mut request_line);
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line.trim() != "" {
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                } else if name.eq_ignore_ascii_case("authorization") {
                    authorization = Some(value.trim().to_string());
                }
            }
            line.clear();
        }
        let mut body = vec![0; length];
        let _ = reader.read_exact(&mut body);
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if let (Ok(mut heard), Ok(mut authorized)) = (self.heard.lock(), self.authorized.lock()) {
            heard.push((request_line.trim().to_string(), body.clone()));
            authorized.push(authorization.clone());
        }
        let answer = self.answer.lock().map(|a| a.clone()).unwrap_or(Answer::Status(500));
        let clip = |wav: Vec<u8>| ("200 OK".to_string(), "audio/wav", wav);
        let refuse = |code: u16| (format!("{code} Nope"), "application/json", br#"{"error":{"message":"the stand-in says no"}}"#.to_vec());
        let (status, ctype, payload) = match answer {
            Answer::Wav(wav) => clip(wav),
            Answer::Status(code) => refuse(code),
            Answer::RefusingInput(input) if body["input"] == input.as_str() => refuse(503),
            Answer::RefusingInput(_) => clip(fake_clip()),
            Answer::Late(delay) => {
                thread::sleep(delay);
                clip(fake_clip())
            }
            Answer::QuotingTheKey => {
                let token = authorization.as_deref().and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("no key was sent");
                ("401 Unauthorized".to_string(), "application/json", json!({ "error": { "message": format!("Incorrect API key provided: {token}.") } }).to_string().into_bytes())
            }
        };
        let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
        let mut out = &stream;
        let _ = out.write_all(head.as_bytes()).and_then(|()| out.write_all(&payload)).and_then(|()| out.flush());
    }

    /// Runs `body` with the fake answering `answer`, and puts the clip back whatever happens.
    fn answering<T>(answer: Answer, body: impl FnOnce(&'static FakeSpeech) -> Result<T, String>) -> Result<T, String> {
        let fake = FakeSpeech::get()?;
        let clip = fake.answer.lock().map_err(|_| "the fake's answer is poisoned")?.clone();
        *fake.answer.lock().map_err(|_| "the fake's answer is poisoned")? = answer;
        let result = body(fake);
        *fake.answer.lock().map_err(|_| "the fake's answer is poisoned")? = clip;
        result
    }

    fn heard(&self) -> Vec<(String, Value)> {
        self.heard.lock().map(|h| h.clone()).unwrap_or_default()
    }

    fn authorized(&self) -> Vec<Option<String>> {
        self.authorized.lock().map(|a| a.clone()).unwrap_or_default()
    }
}

/// An address where nothing listens: a port the system just handed out, closed again.
fn nothing_listening() -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("finding a free port: {e}"))?;
    let port = listener.local_addr().map_err(|e| format!("its address: {e}"))?.port();
    drop(listener);
    Ok(format!("http://127.0.0.1:{port}/v1"))
}

fn a_turn_is_voiced_with_the_words_the_bubble_shows() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    with_stub("voiced", |client| {
        client.say("ok")?;
        client.expect_turn("a two-clause answer in the service's voice", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        client.shutdown()?;
        Ok(())
    })?;
    let heard = fake.heard();
    let asked: Vec<&(String, Value)> = heard[before..].iter().collect();
    let [(line, body)] = asked.as_slice() else {
        return Err(format!("the service should be asked once for one turn, it was asked {} times: {asked:?}", asked.len()));
    };
    if line != "POST /v1/audio/speech HTTP/1.1" {
        return Err(format!("the request should be POST /v1/audio/speech, it was {line:?}"));
    }
    if body["input"] != "收到 我在這裡" || body["response_format"] != "wav" {
        return Err(format!("the service should be asked for a WAV of the words without their markers, it was sent {body}"));
    }
    if !(body["model"].is_string() && body["voice"].is_string()) || body.as_object().map(|o| o.len()) != Some(4) {
        return Err(format!("the body should be exactly model, input, voice and response_format, it was {body}"));
    }
    Ok(String::new())
}

fn with_no_speech_service_the_words_arrive_and_say_how_to_start_it() -> Result<String, String> {
    let url = nothing_listening()?;
    with_core_speaking_to(Mode::Echo { warmup_ms: None }, &url, |client| {
        client.expect_ready()?;
        let sent = Instant::now();
        client.say("有人在嗎")?;
        client.expect_silent("a turn with the service off", 0, "有人在嗎", json!([]), &["本機語音服務沒有開", &url, "serve.py"])?;
        let took = sent.elapsed();
        client.shutdown()?;
        Ok(format!("silent_when_service_off={:.0}ms", ms(took)))
    })
}

fn a_speech_service_that_answers_an_error_is_named() -> Result<String, String> {
    FakeSpeech::answering(Answer::Status(503), |_| {
        with_core(Mode::Echo { warmup_ms: None }, |client| {
            client.expect_ready()?;
            client.say("你好")?;
            client.expect_silent("a 503 from the service", 0, "你好", json!([]), &["HTTP 503", "the stand-in says no"])?;
            client.shutdown()?;
            Ok(String::new())
        })
    })
}

fn audio_in_another_format_is_named_and_not_played() -> Result<String, String> {
    let coarse = checks::wav(1, 1, 16000, 8, &pcm_bytes(&fake_pcm()), b"");
    FakeSpeech::answering(Answer::Wav(coarse), |_| {
        with_core(Mode::Echo { warmup_ms: None }, |client| {
            client.expect_ready()?;
            client.say("你好")?;
            client.expect_silent("an 8-bit clip", 0, "你好", json!([]), &["格式我讀不懂", "8 位元"])?;
            client.shutdown()?;
            Ok(String::new())
        })
    })
}

fn a_clip_with_no_samples_is_silent_and_not_voiced() -> Result<String, String> {
    FakeSpeech::answering(Answer::Wav(checks::wav(1, 1, 16000, 16, &[], b"")), |_| {
        with_core(Mode::Echo { warmup_ms: None }, |client| {
            client.expect_ready()?;
            client.say("你好")?;
            client.expect_silent("an empty clip", 0, "你好", json!([]), &["沒有任何取樣"])?;
            client.shutdown()?;
            Ok(String::new())
        })
    })
}

fn a_speech_url_off_this_machine_is_refused_by_name() -> Result<String, String> {
    with_core_speaking_to(Mode::Echo { warmup_ms: None }, "http://example.com/v1", |client| {
        client.expect_ready()?;
        client.say("你好")?;
        client.expect_silent("a speech URL that is not loopback", 0, "你好", json!([]), &[voice::URL_ENV, "http://example.com/v1"])?;
        client.shutdown()?;
        Ok(String::new())
    })
}

/// Not a real key. Long and distinctive, so finding it anywhere it should not be is no accident.
const KEY: &str = "sk-selftest-7f3a9c1e5b2d4a68";
const KEY_VAR: &str = "VRCHATCORE_SELFTEST_KEY";

/// A core told to use a service with a model, a voice and a key from `KEY_VAR`.
fn custom_env() -> [(&'static str, &'static str); 4] {
    [(voice::MODEL_ENV, "tts-1"), (voice::VOICE_ENV, "alloy"), (voice::KEY_ENV_ENV, KEY_VAR), (KEY_VAR, KEY)]
}

fn a_clip_at_another_rate_is_converted_and_played() -> Result<String, String> {
    // 970 samples at 16 kHz is 1455 at 24 kHz: the fake's clip, at the rate OpenAI answers in.
    let at_24k: Vec<i16> = [vec![0; 480], vec![1000; 480], vec![500; 480], vec![500; 15]].concat();
    FakeSpeech::answering(Answer::Wav(checks::wav(1, 1, 24000, 16, &pcm_bytes(&at_24k), b"")), |_| {
        with_core(Mode::Echo { warmup_ms: None }, |client| {
            client.expect_ready()?;
            client.say("你好")?;
            let got = client.recv("a converted clip")?;
            let envelope = got["voice"]["envelope"].as_array().cloned().unwrap_or_default();
            let level = |i: usize| envelope.get(i).and_then(Value::as_f64).unwrap_or(-1.0);
            let ok = got["voice"]["state"] == "voiced" && got["voice"]["samples"] == 970 && envelope.len() == 3 && level(1) == 1.0 && (0.45..0.55).contains(&level(2));
            if !ok {
                return Err(format!("a 24 kHz clip should arrive as 970 voiced samples at 16 kHz whose third frame is half the second, saw {}", got["voice"]));
            }
            client.expect("followed by turn_end", json!({ "ev": "turn_end" }))?;
            client.shutdown().map(drop)
        })
    })?;
    Ok(String::new())
}

fn a_custom_speech_service_gets_its_model_voice_and_key() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    with_core_configured(Mode::Echo { warmup_ms: None }, &fake.url, &custom_env(), |client| {
        client.expect_ready()?;
        client.say("a")?;
        client.expect_turn("a sentence before any character", "a", json!([]))?;
        client.character("阿花", "愛說話。", Some("hana"))?;
        client.say("b")?;
        client.expect_turn("a sentence as 阿花", "b", json!([]))?;
        client.shutdown()?;
        client.never_said(KEY)
    })?;
    let heard = fake.heard();
    let asked: Vec<&Value> = heard[before..].iter().map(|(_, body)| body).collect();
    let want = [
        json!({ "model": "tts-1", "input": "a", "voice": "alloy", "response_format": "wav" }),
        json!({ "model": "tts-1", "input": "b", "voice": "alloy", "response_format": "wav" }),
    ];
    if asked != want.iter().collect::<Vec<_>>() {
        return Err(format!("both turns should be asked with the model and the voice from the environment, the second not in 阿花's own voice; it was asked {asked:?}"));
    }
    let sent: Vec<Option<String>> = fake.authorized()[before..].to_vec();
    let bearer = Some(format!("Bearer {KEY}"));
    if sent != [bearer.clone(), bearer] {
        return Err(format!("each request should carry the key from {KEY_VAR} as a bearer token; {} of the headers were missing", sent.iter().filter(|a| a.is_none()).count()));
    }
    Ok(String::new())
}

fn without_the_key_variable_every_turn_is_silent_and_says_which() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    let env = [(voice::KEY_ENV_ENV, "VRCHATCORE_SELFTEST_NO_SUCH_KEY")];
    with_core_configured(Mode::Echo { warmup_ms: None }, &fake.url, &env, |client| {
        client.expect_ready()?;
        for word in ["一", "二"] {
            client.say(word)?;
            client.expect_silent("a turn whose key variable is not set", 0, word, json!([]), &[voice::KEY_ENV_ENV, "VRCHATCORE_SELFTEST_NO_SUCH_KEY", "沒有設值"])?;
        }
        client.shutdown().map(drop)
    })?;
    if fake.heard().len() != before {
        return Err("no request should be sent to a service without the key that was asked for".to_string());
    }
    Ok(String::new())
}

fn a_refusal_that_quotes_the_key_is_shown_without_it() -> Result<String, String> {
    FakeSpeech::answering(Answer::QuotingTheKey, |fake| {
        with_core_configured(Mode::Echo { warmup_ms: None }, &fake.url, &custom_env(), |client| {
            client.expect_ready()?;
            client.say("你好")?;
            client.expect_silent("a 401 that quotes the key", 0, "你好", json!([]), &["HTTP 401", "Incorrect API key provided", "***", voice::KEY_ENV_ENV])?;
            client.shutdown()?;
            client.never_said(KEY)
        })
    })?;
    Ok(String::new())
}

fn a_plain_http_service_elsewhere_is_refused_and_the_key_stays_home() -> Result<String, String> {
    with_core_configured(Mode::Echo { warmup_ms: None }, "http://example.com/v1", &custom_env(), |client| {
        client.expect_ready()?;
        client.say("你好")?;
        client.expect_silent("plain http off this machine, with a key", 0, "你好", json!([]), &[voice::URL_ENV, "http://example.com/v1"])?;
        client.shutdown()?;
        client.never_said(KEY)
    })?;
    Ok(String::new())
}

fn an_https_service_that_is_not_there_is_named() -> Result<String, String> {
    let url = nothing_listening()?.replace("http://127.0.0.1", "https://localhost");
    let needles: &[&str] = if cfg!(feature = "tls") { &["連不上", "https://localhost"] } else { &["TLS", "--features tls"] };
    with_core_configured(Mode::Echo { warmup_ms: None }, &url, &custom_env(), |client| {
        client.expect_ready()?;
        client.say("你好")?;
        client.expect_silent("an https service with nothing behind it", 0, "你好", json!([]), needles)?;
        client.shutdown()?;
        client.never_said(KEY)
    })?;
    Ok(String::new())
}

fn claude_is_started_without_the_speech_key() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    let log = std::env::temp_dir().join(format!("vrchatcore-selftest-{}-key.log", std::process::id()));
    with_core_configured(Mode::Stub { log, model: None }, &fake.url, &custom_env(), |client| {
        client.expect_ready()?;
        let start = client.wait_for_start(1)?;
        if start["sees_key"] != false {
            return Err(format!("claude should not be started with the variable that holds the speech key, the stand-in saw {}", start["sees_key"]));
        }
        client.say("ok")?;
        client.expect_turn("a turn through the stand-in", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        client.shutdown().map(drop)
    })?;
    if fake.authorized()[before..] != [Some(format!("Bearer {KEY}"))] {
        return Err("the core itself should still have sent the key, or the test shows nothing".to_string());
    }
    Ok(String::new())
}

/// The stand-in's "long" answer in one piece.
const LONG: &str = "[self]好熱喔，我懂那種不想動的感覺。 [offer]要不要先待在家吹冷氣，喝點冰的， [nod]等傍晚涼一點再出去走走？";

/// The stand-in's "long" answer in the fake service's voice. The first part is the starting pace's
/// alone: 好熱喔，(3 syllables, 0.63 s) cannot cover the 10 after it (2.04 s), and up to 感覺。
/// (13, 2.73 s) the rest can follow. Behind the fake's 61 ms clip each later part is one atom.
fn long_parts() -> Vec<(&'static str, Value)> {
    vec![
        ("[self]好熱喔，我懂那種不想動的感覺。", json!(["self"])),
        ("[offer]要不要先待在家吹冷氣，", json!(["offer"])),
        ("喝點冰的，", json!([])),
        ("[nod]等傍晚涼一點再出去走走？", json!(["nod"])),
    ]
}

fn inputs_since(fake: &FakeSpeech, before: usize) -> Vec<Value> {
    fake.heard()[before..].iter().map(|(_, body)| body["input"].clone()).collect()
}

fn a_long_answer_arrives_in_parts_each_in_its_own_voice() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    let took = with_stub("parts", |client| {
        let sent = Instant::now();
        client.say("long")?;
        client.expect_parts("a long answer", &long_parts())?;
        let took = sent.elapsed();
        client.say("ok")?;
        client.expect_turn("a short answer after it", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        client.shutdown()?;
        Ok(took)
    })?;
    let asked = inputs_since(fake, before);
    let want = ["好熱喔，我懂那種不想動的感覺。", "要不要先待在家吹冷氣，", "喝點冰的，", "等傍晚涼一點再出去走走？", "收到 我在這裡"];
    if asked != want.map(Value::from) {
        return Err(format!("the service should be asked once per part for that part's words without markers, {want:?}; it was asked for {asked:?}"));
    }
    Ok(format!("long_answer_in_4_parts={:.0}ms", ms(took)))
}

fn a_long_answer_with_no_voice_arrives_whole_and_silent() -> Result<String, String> {
    let tags = json!(["self", "offer", "nod"]);
    let url = nothing_listening()?;
    with_stub_at("unvoiced", None, &url, |client| {
        client.say("long")?;
        client.expect_silent("a long answer with the service off", 0, LONG, tags.clone(), &["本機語音服務沒有開"])?;
        client.shutdown().map(drop)
    })?;
    // A service that answers but refuses is asked once for the turn, not once per part.
    FakeSpeech::answering(Answer::Status(503), |fake| {
        let before = fake.heard().len();
        with_stub("refused", |client| {
            client.say("long")?;
            client.expect_silent("a long answer the service refuses", 0, LONG, tags.clone(), &["HTTP 503"])?;
            client.shutdown().map(drop)
        })?;
        match inputs_since(fake, before).len() {
            1 => Ok(String::new()),
            n => Err(format!("a turn whose first part the service refused should ask it once, it asked {n} times")),
        }
    })
}

fn a_part_the_service_refuses_takes_the_rest_of_the_answer_silent() -> Result<String, String> {
    FakeSpeech::answering(Answer::RefusingInput("喝點冰的，".to_string()), |fake| {
        let before = fake.heard().len();
        with_stub("partly", |client| {
            client.say("long")?;
            let parts = long_parts();
            for (part, (text, tags)) in parts.iter().take(2).enumerate() {
                client.expect_voiced("the parts before the refused one", part, text, tags.clone())?;
            }
            client.expect_silent(
                "the refused part, with everything after it",
                2,
                "喝點冰的， [nod]等傍晚涼一點再出去走走？",
                json!(["nod"]),
                &["HTTP 503"],
            )?;
            client.shutdown().map(drop)
        })?;
        let asked = inputs_since(fake, before);
        let want = ["好熱喔，我懂那種不想動的感覺。", "要不要先待在家吹冷氣，", "喝點冰的，"];
        if asked != want.map(Value::from) {
            return Err(format!("after a refused part the service should be asked nothing more, it was asked for {asked:?}"));
        }
        Ok(String::new())
    })
}

fn what_one_turn_learns_about_the_service_paces_the_next() -> Result<String, String> {
    // 150 ms for 61 ms of audio: slower than real time.
    FakeSpeech::answering(Answer::Late(Duration::from_millis(150)), |_| {
        with_stub("slow", |client| {
            client.say("long")?;
            client.expect_parts("a long answer on a fresh pace, which knows nothing of the service yet", &long_parts())?;
            client.say("long")?;
            client.expect_turn("the same answer once the pace has learned the service is slow", LONG, json!(["self", "offer", "nod"]))?;
            client.shutdown().map(drop)
        })?;
        Ok(String::new())
    })
}

/// The `voice` field of every request the fake service heard since `before`.
fn voices_since(fake: &FakeSpeech, before: usize) -> Vec<Value> {
    fake.heard()[before..].iter().map(|(_, body)| body["voice"].clone()).collect()
}

/// What the stand-in was started with as the system prompt.
fn prompt_of(start: &Value) -> Option<String> {
    let args = start["args"].as_array()?;
    let at = args.iter().position(|a| a == "--append-system-prompt")?;
    args.get(at + 1)?.as_str().map(str::to_string)
}

fn a_new_prompt_restarts_claude_and_a_new_voice_alone_does_not() -> Result<String, String> {
    with_stub("character", |client| {
        let first = client.wait_for_start(1)?;
        if prompt_of(&first) != Some(reply::prompt(&Character::standard(), reply::Relaying::Off)) {
            return Err(format!("before any character is sent she is 小克, the stand-in was started with {:?}", prompt_of(&first)));
        }

        // Lines, quotes, a backslash and an emoji: the persona crosses a Windows command line.
        let persona = "第一行\n第二行 \"引號\" \\ 反斜線 😀";
        let flower = Character::new("阿花", persona, None)?;
        client.character("阿花", persona, None)?;
        let second = client.wait_for_start(2)?;
        if prompt_of(&second) != Some(reply::prompt(&flower, reply::Relaying::Off)) {
            return Err(format!("the second claude should be started with 阿花's prompt, it was started with {:?}", prompt_of(&second)));
        }
        wait_gone(pid_of(&first)?).map_err(|e| format!("the claude of the character before was left running: {e}"))?;
        client.say("ok")?;
        client.expect_turn("a sentence after the change", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;

        // The same prompt in another voice is the same claude.
        client.character("阿花", persona, Some("hana"))?;
        client.say("ok")?;
        client.expect_turn("a sentence after a change of voice alone", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        let starts = client.starts().len();
        if starts != 2 {
            return Err(format!("a change of voice alone should not restart claude, the stand-in was started {starts} times"));
        }

        // Back to the default prompt is a change of prompt like any other.
        client.character("小克", "你住在使用者的桌面角落，講話短、口語，預設用繁體中文，對方用別的語言就用那個語言。", None)?;
        let third = client.wait_for_start(3)?;
        if prompt_of(&third) != Some(reply::prompt(&Character::standard(), reply::Relaying::Off)) {
            return Err(format!("the third claude should be 小克's again, it was started with {:?}", prompt_of(&third)));
        }

        // The longest persona a profile may carry, on a command line with the schema and the rules.
        let longest = "字".repeat(2000);
        client.character("巨大", &longest, None)?;
        let fourth = client.wait_for_start(4)?;
        if prompt_of(&fourth) != Some(reply::prompt(&Character::new("巨大", &longest, None)?, reply::Relaying::Off)) {
            return Err("a persona of 2000 characters did not reach claude intact".to_string());
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_character_changes_the_voice_the_service_is_asked_for() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    with_core(Mode::Echo { warmup_ms: None }, |client| {
        client.expect_ready()?;
        client.say("a")?;
        client.expect_turn("a sentence before any character", "a", json!([]))?;
        client.character("阿花", "愛說話。", Some("hana"))?;
        client.say("b")?;
        client.expect_turn("a sentence as 阿花", "b", json!([]))?;
        client.character("小花", "愛說話。", None)?;
        client.say("c")?;
        client.expect_turn("a sentence as a character with no voice of her own", "c", json!([]))?;
        client.shutdown().map(drop)
    })?;
    let want = [DEFAULT_VOICE, "hana", DEFAULT_VOICE].map(Value::from);
    let heard = voices_since(fake, before);
    if heard != want {
        return Err(format!("the service should be asked for the voices {want:?} in turn, it was asked for {heard:?}"));
    }
    Ok(String::new())
}

fn a_character_that_breaks_a_limit_is_an_error_and_changes_nothing() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    with_stub("limits", |client| {
        let bad: [(Value, &[&str]); 7] = [
            (json!({ "op": "character", "name": "字".repeat(33), "persona": "好" }), &["name is 33 characters"]),
            (json!({ "op": "character", "name": "阿花", "persona": "" }), &["persona is empty"]),
            (json!({ "op": "character", "name": "阿花", "persona": "字".repeat(2001) }), &["persona is 2001 characters"]),
            (json!({ "op": "character", "name": "阿花", "persona": "好", "voice": "../x" }), &["not a voice id"]),
            (json!({ "op": "character", "name": "阿花", "persona": "好", "voice": "Hana" }), &["not a voice id"]),
            (json!({ "op": "character", "name": "阿花", "persona": "好", "voice": 7 }), &["expected a string"]),
            (json!({ "op": "character", "name": "阿花" }), &["missing field `persona`"]),
        ];
        for (line, needles) in &bad {
            client.send(line.to_string().as_bytes())?;
            client.expect_error(&format!("a character line {}", line.to_string().chars().take(40).collect::<String>()), needles)?;
        }
        // The round trip is also the proof that nothing was queued behind the refusals: the engine
        // does its jobs in order, so a restart or a new voice would have happened before this answer.
        client.say("ok")?;
        client.expect_turn("a sentence after the refusals", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        let starts = client.starts();
        if starts.len() != 1 || prompt_of(&starts[0]) != Some(reply::prompt(&Character::standard(), reply::Relaying::Off)) {
            return Err(format!("refused characters should leave 小克 and her claude as they were, the starts are {starts:?}"));
        }
        client.shutdown().map(drop)
    })?;
    let heard = voices_since(fake, before);
    if heard != [Value::from(DEFAULT_VOICE)] {
        return Err(format!("refused characters should leave the default voice, the service was asked for {heard:?}"));
    }
    Ok(String::new())
}

fn a_greeting_is_voiced_in_the_current_voice_without_claude() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    // `claude` cannot be started at all here, so a greeting that reached for it would be the error
    // the second sentence gets.
    with_core(Mode::Absent { required: true }, |client| {
        client.expect_ready()?;
        client.greet("嗨，我在這裡。")?;
        client.expect_turn("a greeting as 小克", "嗨，我在這裡。", json!([]))?;
        client.character("阿花", "愛說話。", Some("hana"))?;
        client.greet("我是阿花")?;
        client.expect_turn("a greeting as 阿花, which follows the change that was sent before it", "我是阿花", json!([]))?;
        client.say("anyone there?")?;
        client.expect_error("a sentence, which does need claude", &["claude was not found"])?;
        client.shutdown().map(drop)
    })?;
    let asked: Vec<(Value, Value)> = fake.heard()[before..].iter().map(|(_, body)| (body["input"].clone(), body["voice"].clone())).collect();
    let want = [("嗨，我在這裡。", DEFAULT_VOICE), ("我是阿花", "hana")].map(|(text, voice)| (Value::from(text), Value::from(voice)));
    if asked != want {
        return Err(format!("the service should be asked for each greeting in its character's voice, {want:?}; it was asked {asked:?}"));
    }
    Ok(String::new())
}

fn a_greeting_with_no_speech_service_is_silent_and_still_ends_the_turn() -> Result<String, String> {
    let url = nothing_listening()?;
    with_core_speaking_to(Mode::Echo { warmup_ms: None }, &url, |client| {
        client.expect_ready()?;
        client.greet("嗨")?;
        client.expect_silent("a greeting with the service off", 0, "嗨", json!([]), &["本機語音服務沒有開"])?;
        client.shutdown().map(drop)
    })?;
    Ok(String::new())
}

fn a_bad_greeting_is_an_error_and_says_nothing() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    with_core(Mode::Echo { warmup_ms: None }, |client| {
        client.expect_ready()?;
        let bad: [(Value, &[&str]); 4] = [
            (json!({ "op": "greet", "text": "字".repeat(201) }), &["text is 201 characters"]),
            (json!({ "op": "greet", "text": "" }), &["text is empty"]),
            (json!({ "op": "greet", "text": "[nod]嗨" }), &["text contains"]),
            (json!({ "op": "greet" }), &["missing field `text`"]),
        ];
        for (line, needles) in &bad {
            client.send(line.to_string().as_bytes())?;
            client.expect_error(&format!("a greeting line {}", line.to_string().chars().take(40).collect::<String>()), needles)?;
        }
        client.say("still here")?;
        client.expect_turn("a sentence after the refusals", "still here", json!([]))?;
        client.shutdown().map(drop)
    })?;
    let asked = fake.heard().len() - before;
    if asked != 1 {
        return Err(format!("refused greetings should not be voiced, the service was asked {asked} times for one sentence"));
    }
    Ok(String::new())
}

/// A greeting or a character change that arrives while a turn is being spoken does not cut into
/// it and is not lost: the turn goes out whole, as the character it was asked of, and then the
/// change and the greeting happen in the order they were sent.
fn a_character_and_a_greeting_wait_for_the_turn_in_flight() -> Result<String, String> {
    let fake = FakeSpeech::get()?;
    let before = fake.heard().len();
    with_stub("queued", |client| {
        client.say("long")?;
        client.character("阿花", "愛說話。", Some("hana"))?;
        client.greet("我是阿花")?;
        client.expect_parts("the long answer, in full and in 小克's voice", &long_parts())?;
        client.expect_turn("then the greeting, in 阿花's", "我是阿花", json!([]))?;
        client.say("ok")?;
        client.expect_turn("a sentence once 阿花 is in place", "[nod]收到 [offer]我在這裡", json!(["nod", "offer"]))?;
        let starts = client.starts();
        if starts.len() != 2 || prompt_of(&starts[1]) != Some(reply::prompt(&Character::new("阿花", "愛說話。", None)?, reply::Relaying::Off)) {
            return Err(format!("claude should have been restarted once, for 阿花, the starts are {starts:?}"));
        }
        client.shutdown().map(drop)
    })?;
    let want: Vec<Value> = [DEFAULT_VOICE, DEFAULT_VOICE, DEFAULT_VOICE, DEFAULT_VOICE, "hana", "hana"].map(Value::from).to_vec();
    let heard = voices_since(fake, before);
    if heard != want {
        return Err(format!("four parts in the default voice and then two in 阿花's were expected, the service was asked for {heard:?}"));
    }
    Ok(String::new())
}

/// The chat service, played by a listener in this process, one thread per request so a request it
/// holds does not hold the next. Like the stand-in for `claude`, it answers by the last thing the
/// user said: a few words are cases under test and anything else is the two-clause answer.
struct FakeChat {
    url: String,
    /// The request line, the JSON body and the `Authorization` header of every request, in order.
    heard: Mutex<Vec<(String, Value, Option<String>)>>,
}

/// `{clauses, language, affect}` as the model writes it, the answer the core is to parse.
fn chat_answer(list: &[(&str, &str)]) -> String {
    let clauses: Vec<Value> = list.iter().map(|(text, gesture)| json!({ "text": text, "gesture": gesture })).collect();
    json!({ "clauses": clauses, "language": "zh-TW", "affect": "calm" }).to_string()
}

impl FakeChat {
    fn get() -> Result<&'static FakeChat, String> {
        static FAKE: OnceLock<FakeChat> = OnceLock::new();
        if let Some(fake) = FAKE.get() {
            return Ok(fake);
        }
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("starting the stand-in chat service: {e}"))?;
        let port = listener.local_addr().map_err(|e| format!("its address: {e}"))?.port();
        let fake = FAKE.get_or_init(|| FakeChat { url: format!("http://127.0.0.1:{port}/v1"), heard: Mutex::new(Vec::new()) });
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || fake.answer_one(stream));
            }
        });
        Ok(fake)
    }

    fn answer_one(&self, stream: TcpStream) {
        let mut reader = BufReader::new(&stream);
        let mut request_line = String::new();
        let mut length = 0;
        let mut authorization = None;
        let mut line = String::new();
        let _ = reader.read_line(&mut request_line);
        while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line.trim() != "" {
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                } else if name.eq_ignore_ascii_case("authorization") {
                    authorization = Some(value.trim().to_string());
                }
            }
            line.clear();
        }
        let mut body = vec![0; length];
        let _ = reader.read_exact(&mut body);
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if let Ok(mut heard) = self.heard.lock() {
            heard.push((request_line.trim().to_string(), body.clone(), authorization.clone()));
        }
        let said = body["messages"].as_array().and_then(|m| m.last()).and_then(|m| m["content"].as_str()).unwrap_or("").to_string();
        let message = |content: Value| json!({ "choices": [{ "index": 0, "message": content, "finish_reason": "stop" }] });
        let (status, payload) = if request_line.starts_with("GET /v1/models ") {
            ("200 OK".to_string(), json!({ "object": "list", "data": [{ "id": "b-model" }, { "id": "a-model" }, { "id": "not a name" }] }))
        } else {
            match said.as_str() {
                "four" => ("200 OK".to_string(), message(json!({ "role": "assistant", "content": chat_answer(&[("一", "nod"), ("二", "none"), ("三", "nod"), ("四", "nod")]) }))),
                "tool" => (
                    "200 OK".to_string(),
                    message(json!({ "role": "assistant", "content": null, "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "read_mail", "arguments": "{}" } }] })),
                ),
                "refuse" => ("200 OK".to_string(), message(json!({ "role": "assistant", "content": null, "refusal": "I can't help with that." }))),
                "hang" => {
                    thread::sleep(Duration::from_secs(30));
                    ("200 OK".to_string(), message(json!({ "role": "assistant", "content": chat_answer(&[("太晚了", "none")]) })))
                }
                "quote" => {
                    let token = authorization.as_deref().and_then(|a| a.strip_prefix("Bearer ")).unwrap_or("no key was sent");
                    ("401 Unauthorized".to_string(), json!({ "error": { "message": format!("Incorrect API key provided: {token}.") } }))
                }
                other => {
                    let answer = match other.strip_prefix("echo:") {
                        Some(echoed) => chat_answer(&[(echoed, "none")]),
                        None => chat_answer(&[("收到", "nod"), ("我在這裡", "offer")]),
                    };
                    ("200 OK".to_string(), message(json!({ "role": "assistant", "content": answer })))
                }
            }
        };
        let payload = payload.to_string();
        let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
        let mut out = &stream;
        let _ = out.write_all(head.as_bytes()).and_then(|()| out.write_all(payload.as_bytes())).and_then(|()| out.flush());
    }

    fn heard(&self) -> Vec<(String, Value, Option<String>)> {
        self.heard.lock().map(|h| h.clone()).unwrap_or_default()
    }

    /// The bodies of the chat requests heard since `before`, models listings left out.
    fn asked_since(&self, before: usize) -> Vec<Value> {
        self.heard()[before..].iter().filter(|(line, ..)| line.starts_with("POST ")).map(|(_, body, _)| body.clone()).collect()
    }
}

/// The `content` of each message of one request, by role: `("user", "ok")`.
fn messages_of(body: &Value) -> Vec<(String, String)> {
    let each = body["messages"].as_array().cloned().unwrap_or_default();
    each.iter().map(|m| (m["role"].as_str().unwrap_or("?").to_string(), m["content"].as_str().unwrap_or("?").to_string())).collect()
}

fn with_openai<T>(url: &str, env: &[(&str, &str)], body: impl FnOnce(&mut Client) -> Result<T, String>) -> Result<T, String> {
    with_core_configured(Mode::Openai { url: url.to_string() }, &FakeSpeech::get()?.url, env, |client| {
        client.expect_ready()?;
        body(client)
    })
}

/// The two-clause answer the fake gives to anything it has no case for.
const OK_TURN: &str = "[nod]收到 [offer]我在這裡";

fn an_openai_answer_reaches_the_wire_with_its_gestures_as_tags() -> Result<String, String> {
    let fake = FakeChat::get()?;
    let before = fake.heard().len();
    with_openai(&fake.url, &[], |client| {
        client.say("ok")?;
        client.expect_turn("a two-clause answer from the HTTP engine", OK_TURN, json!(["nod", "offer"]))?;
        client.shutdown().map(drop)
    })?;
    let heard = fake.heard()[before..].to_vec();
    let [(line, body, authorization)] = heard.as_slice() else {
        return Err(format!("one sentence should be one request, the service heard {}", heard.len()));
    };
    if line != "POST /v1/chat/completions HTTP/1.1" {
        return Err(format!("the request should be POST /v1/chat/completions, it was {line:?}"));
    }
    let mut keys: Vec<&str> = body.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    keys.sort_unstable();
    if keys != ["messages", "model", "reasoning_effort", "response_format", "stream"] {
        return Err(format!("the body should be exactly messages, model, reasoning_effort, response_format and stream, with no tools and no tool_choice; it has {keys:?}"));
    }
    let want_format = json!({ "type": "json_schema", "json_schema": { "name": "reply", "schema": reply::schema(reply::Relaying::Off), "strict": true } });
    if body["model"] != CHAT_MODEL || body["stream"] != false || body["response_format"] != want_format || body["reasoning_effort"] != "none" {
        return Err(format!("the body should ask {CHAT_MODEL} for the reply schema, unstreamed and without thinking, it was {body}"));
    }
    let want = vec![("system".to_string(), reply::prompt(&Character::standard(), reply::Relaying::Off)), ("user".to_string(), "ok".to_string())];
    if messages_of(body) != want {
        return Err(format!("the messages should be 小克's prompt and the sentence, they were {:?}", messages_of(body)));
    }
    if authorization.is_some() {
        return Err("with no key variable named, no Authorization should be sent".to_string());
    }
    Ok(String::new())
}

fn an_openai_answer_the_core_refuses_is_named_and_not_remembered() -> Result<String, String> {
    let fake = FakeChat::get()?;
    let before = fake.heard().len();
    with_openai(&fake.url, &[], |client| {
        for (word, needles) in [("four", &["4 clauses", "at most 3"][..]), ("tool", &["呼叫工具", "沒有任何工具"][..]), ("refuse", &["拒絕回答", "can't help"][..])] {
            client.say(word)?;
            client.expect_error(&format!("the service answers {word:?}"), needles)?;
        }
        client.say("ok")?;
        client.expect_turn("a sentence after the refused answers", OK_TURN, json!(["nod", "offer"]))?;
        client.shutdown().map(drop)
    })?;
    let asked = fake.asked_since(before);
    let last = asked.last().map(messages_of).unwrap_or_default();
    if last.len() != 2 {
        return Err(format!("refused answers should not be sent back as history, the last request was {last:?}"));
    }
    Ok(String::new())
}

fn an_openai_service_that_is_not_running_is_named() -> Result<String, String> {
    let url = nothing_listening()?;
    let took = with_openai(&url, &[], |client| {
        let sent = Instant::now();
        client.say("有人在嗎")?;
        client.expect_error("a sentence with the chat service off", &["本機聊天服務沒有開", &url, "Ollama"])?;
        let took = sent.elapsed();
        client.shutdown()?;
        Ok(took)
    })?;
    Ok(format!("chat_service_off_to_error={:.0}ms", ms(took)))
}

fn the_history_sent_is_the_last_eight_turns() -> Result<String, String> {
    let fake = FakeChat::get()?;
    let before = fake.heard().len();
    with_openai(&fake.url, &[], |client| {
        for n in 1..=10 {
            let word = format!("echo:{n}");
            client.say(&word)?;
            client.expect_turn(&format!("turn {n}"), &n.to_string(), json!([]))?;
        }
        client.say("ok")?;
        client.expect_turn("the eleventh turn", OK_TURN, json!(["nod", "offer"]))?;
        client.shutdown().map(drop)
    })?;
    let asked = fake.asked_since(before);
    let last = asked.last().map(messages_of).unwrap_or_default();
    let mut want = vec![("system".to_string(), reply::prompt(&Character::standard(), reply::Relaying::Off))];
    for n in 3..=10 {
        want.push(("user".to_string(), format!("echo:{n}")));
        want.push(("assistant".to_string(), chat_answer(&[(&n.to_string(), "none")])));
    }
    want.push(("user".to_string(), "ok".to_string()));
    if last != want {
        let users: Vec<&str> = last.iter().filter(|(role, _)| role == "user").map(|(_, text)| text.as_str()).collect();
        return Err(format!("the eleventh request should carry turns 3 to 10 with their answers as written, it carried {} messages, users {users:?}", last.len()));
    }
    Ok(String::new())
}

fn a_new_character_starts_the_history_over() -> Result<String, String> {
    let fake = FakeChat::get()?;
    let before = fake.heard().len();
    with_openai(&fake.url, &[], |client| {
        client.say("echo:記得我")?;
        client.expect_turn("a turn as 小克", "記得我", json!([]))?;
        client.character("阿花", "愛說話。", None)?;
        client.say("ok")?;
        client.expect_turn("a turn as 阿花", OK_TURN, json!(["nod", "offer"]))?;
        client.character("阿花", "愛說話。", Some("hana"))?;
        client.say("echo:還在")?;
        client.expect_turn("a turn after a change of voice alone", "還在", json!([]))?;
        client.shutdown().map(drop)
    })?;
    let asked: Vec<Vec<(String, String)>> = fake.asked_since(before).iter().map(messages_of).collect();
    let flower = reply::prompt(&Character::new("阿花", "愛說話。", None)?, reply::Relaying::Off);
    let as_flower = vec![("system".to_string(), flower.clone()), ("user".to_string(), "ok".to_string())];
    if asked.get(1) != Some(&as_flower) {
        return Err(format!("阿花's first request should be her prompt and the sentence alone, it was {:?}", asked.get(1)));
    }
    let after_voice = asked.get(2).map(|m| m.iter().map(|(role, _)| role.as_str()).collect::<Vec<_>>());
    if after_voice != Some(vec!["system", "user", "assistant", "user"]) {
        return Err(format!("a change of voice alone keeps her history, the request after it was {:?}", asked.get(2)));
    }
    Ok(String::new())
}

fn the_chat_key_is_a_header_and_never_a_word_anyone_reads() -> Result<String, String> {
    let fake = FakeChat::get()?;
    let before = fake.heard().len();
    let env = [(chat::KEY_ENV_ENV, KEY_VAR), (KEY_VAR, KEY)];
    with_openai(&fake.url, &env, |client| {
        client.say("ok")?;
        client.expect_turn("a turn with a key", OK_TURN, json!(["nod", "offer"]))?;
        client.say("quote")?;
        client.expect_error("a 401 that quotes the key", &["HTTP 401", "Incorrect API key provided", "***", chat::KEY_ENV_ENV])?;
        client.shutdown()?;
        client.never_said(KEY)
    })?;
    let heard = fake.heard()[before..].to_vec();
    let bearer = Some(format!("Bearer {KEY}"));
    if heard.len() != 2 || heard.iter().any(|(_, _, authorization)| *authorization != bearer) {
        return Err(format!("each request should carry the key from {KEY_VAR} as a bearer token, the headers were {:?}", heard.iter().map(|h| h.2.is_some()).collect::<Vec<_>>()));
    }
    if let Some((line, ..)) = heard.iter().find(|(line, body, _)| line.contains(KEY) || body.to_string().contains(KEY)) {
        return Err(format!("the key should be in the header only, it was in the request line or the body of {line:?}"));
    }
    Ok(String::new())
}

fn claude_is_started_without_the_chat_key() -> Result<String, String> {
    let log = std::env::temp_dir().join(format!("vrchatcore-selftest-{}-chatkey.log", std::process::id()));
    let env = [(chat::KEY_ENV_ENV, KEY_VAR), (KEY_VAR, KEY)];
    with_core_configured(Mode::Stub { log, model: None }, &FakeSpeech::get()?.url, &env, |client| {
        client.expect_ready()?;
        let start = client.wait_for_start(1)?;
        if start["sees_chat_key"] != false {
            return Err(format!("claude should not be started with the variable that holds the chat key, the stand-in saw {}", start["sees_chat_key"]));
        }
        client.say("ok")?;
        client.expect_turn("a turn through the stand-in", OK_TURN, json!(["nod", "offer"]))?;
        client.shutdown().map(drop)
    })?;
    Ok(String::new())
}

/// The engine's own wait, called here directly so the limit can be short: the core's is 120 s.
fn a_chat_turn_ends_at_its_limit_or_at_a_stop() -> Result<String, String> {
    let fake = FakeChat::get()?;
    let env = chat::ChatEnv { url: Some(fake.url.clone().into()), model: Some(CHAT_MODEL.into()), key_env: None };
    let history = chat::History::default();
    let never = std::sync::atomic::AtomicBool::new(false);

    let short = chat::Client::new(&env, &|_| None, Duration::from_secs(1));
    let began = Instant::now();
    let got = short.ask("prompt", &history, "hang", &never);
    let limited = began.elapsed();
    match got {
        Err(claude::Failure::Broken(why)) if why.contains("1 秒內沒有回答") => {}
        Err(claude::Failure::Broken(why)) => return Err(format!("a turn past its limit should say so, it said {why:?}")),
        Err(claude::Failure::Stopped) => return Err("a turn past its limit is not a stop".to_string()),
        Ok(text) => return Err(format!("a turn that never answers should not have answered: {text}")),
    }
    if !(Duration::from_millis(900)..Duration::from_secs(3)).contains(&limited) {
        return Err(format!("a turn with a 1 s limit should end after about 1 s, it ended after {limited:?}"));
    }

    let long = chat::Client::new(&env, &|_| None, Duration::from_secs(30));
    let stop = std::sync::atomic::AtomicBool::new(false);
    let began = Instant::now();
    let got = thread::scope(|scope| {
        scope.spawn(|| {
            thread::sleep(Duration::from_millis(100));
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        long.ask("prompt", &history, "hang", &stop)
    });
    let stopped = began.elapsed();
    if !matches!(got, Err(claude::Failure::Stopped)) || stopped > Duration::from_secs(1) {
        return Err(format!("a stop during the wait should end it at once as a stop, it took {stopped:?} and gave {:?}", got.map_err(|e| matches!(e, claude::Failure::Stopped))));
    }
    Ok(format!("chat_limit={:.0}ms chat_stop={:.0}ms", ms(limited), ms(stopped)))
}

fn a_chat_turn_in_flight_does_not_keep_the_core_from_leaving() -> Result<String, String> {
    let fake = FakeChat::get()?;
    let took = with_openai(&fake.url, &[], |client| {
        client.say("hang")?;
        thread::sleep(Duration::from_millis(300));
        client.shutdown()
    })?;
    Ok(format!("leave_mid_chat_turn={:.0}ms", ms(took)))
}

/// `vrchatcore models` as the settings window's command runs it: the four variables in, one line out.
fn list_models(env: &[(&str, &str)]) -> Result<(bool, Value), String> {
    let exe = std::env::current_exe().map_err(|e| format!("locating own executable: {e}"))?;
    let mut cmd = Command::new(&exe);
    cmd.arg("models");
    for name in [chat::URL_ENV, chat::MODEL_ENV, chat::KEY_ENV_ENV] {
        cmd.env_remove(name);
    }
    let out = cmd.envs(env.iter().copied()).output().map_err(|e| format!("running {} models: {e}", exe.display()))?;
    let line = String::from_utf8_lossy(&out.stdout).into_owned();
    if line.lines().count() != 1 || line.contains(KEY) || String::from_utf8_lossy(&out.stderr).contains(KEY) {
        return Err(format!("models should print one line and no key, it printed {line:?}"));
    }
    let value = serde_json::from_str(line.trim()).map_err(|e| format!("models printed something that is not JSON ({e}): {line:?}"))?;
    Ok((out.status.success(), value))
}

fn the_models_are_listed_for_the_settings_window() -> Result<String, String> {
    let fake = FakeChat::get()?;
    let before = fake.heard().len();
    let (ok, listed) = list_models(&[(chat::URL_ENV, &fake.url), (chat::KEY_ENV_ENV, KEY_VAR), (KEY_VAR, KEY)])?;
    if !ok || listed != json!({ "models": ["a-model", "b-model"] }) {
        return Err(format!("the service's models should be listed sorted, without one that is not a name, it printed {listed}"));
    }
    let heard = fake.heard()[before..].to_vec();
    if heard.len() != 1 || heard[0].0 != "GET /v1/models HTTP/1.1" || heard[0].2 != Some(format!("Bearer {KEY}")) {
        return Err(format!("the listing should be one GET /v1/models with the key as a bearer token, it was {:?}", heard.iter().map(|h| &h.0).collect::<Vec<_>>()));
    }
    let off = nothing_listening()?;
    let (ok, listed) = list_models(&[(chat::URL_ENV, &off)])?;
    let why = listed["error"].as_str().unwrap_or_default();
    if ok || !why.contains("本機聊天服務沒有開") || !why.contains(&off) {
        return Err(format!("a service that is not running should be an error that says so, it printed {listed}"));
    }
    let (ok, listed) = list_models(&[(chat::URL_ENV, "http://example.com/v1")])?;
    let why = listed["error"].as_str().unwrap_or_default();
    if ok || !why.contains(chat::URL_ENV) || !why.contains("https://") {
        return Err(format!("plain http off this machine should be refused by the rule, it printed {listed}"));
    }
    Ok(String::new())
}

// Relaying, with the stand-in as the character and as the courier. "Sent" is what the courier ran
// after the gate, in the file `stub::SENT_ENV` names.

const SUMMARY: &str = "使用者透過 AImate 轉話";
const BODY: &str = "轉話測試：設定頁的主題做好了。\n可以看了。";
const TARGET: &str = "AImate桌寵進度整理";
const TARGET_ADDRESS: &str = "AImate桌寵進度整理 [77db53]";

/// The files a relaying core's stand-ins write.
struct Relay {
    sent: PathBuf,
    heard: PathBuf,
}

impl Relay {
    fn lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// What the courier ran after the gate, ListAgents aside.
    fn sent(&self) -> Vec<Value> {
        Self::lines(&self.sent)
    }

    /// The texts `who` ("character" or "courier") was told, in order.
    fn heard(&self, who: &str) -> Vec<String> {
        Self::lines(&self.heard).iter().filter(|l| l["who"] == who).filter_map(|l| l["text"].as_str().map(str::to_string)).collect()
    }
}

fn with_courier<T>(name: &str, behaviour: &'static str, step_ms: Option<u64>, env: &[(&str, &str)], body: impl FnOnce(&mut Client, &Relay) -> Result<T, String>) -> Result<T, String> {
    let base = std::env::temp_dir().join(format!("vrchatcore-selftest-{}-relay-{name}", std::process::id()));
    let relay = Relay { sent: base.with_extension("sent"), heard: base.with_extension("heard") };
    let mode = Mode::Courier { log: base.with_extension("log"), sent: relay.sent.clone(), heard: relay.heard.clone(), behaviour, step_ms };
    let done = with_core_configured(mode, &FakeSpeech::get()?.url, env, |client| {
        client.expect_ready()?;
        body(client, &relay)
    });
    let _ = std::fs::remove_file(&relay.sent);
    let _ = std::fs::remove_file(&relay.heard);
    done
}

impl Client {
    fn relay(&mut self, nonce: &str, title: &str, body: &str) -> Result<(), String> {
        self.send(json!({ "op": "relay", "nonce": nonce, "title": title, "body": body, "summary": SUMMARY }).to_string().as_bytes())
    }

    /// Every line up to the `relayed` for `nonce`, and that line. Each line has to come within the
    /// step limit, which is longer than any stand-in run.
    fn until_relayed(&mut self, nonce: &str) -> Result<(Vec<Value>, Value), String> {
        let mut before = Vec::new();
        loop {
            let line = self.recv(&format!("the relayed for {nonce}"))?;
            if line["ev"] == "relayed" && line["nonce"] == nonce {
                return Ok((before, line));
            }
            before.push(line);
        }
    }

    /// The `relayed` for `nonce`, which must be `outcome` with `gated`, after exactly the steps
    /// given. `elapsed_ms` and `cost_usd` are only checked for their type.
    fn expect_relayed(&mut self, nonce: &str, steps: &[Value], outcome: Value, gated: bool) -> Result<Value, String> {
        let (before, line) = self.until_relayed(nonce)?;
        if before != steps {
            return Err(format!("{nonce}: the lines before relayed should be {steps:?}, they were {before:?}"));
        }
        let shaped = line.as_object().is_some_and(|o| o.len() == 6) && line["elapsed_ms"].is_u64() && (line["cost_usd"].is_f64() || line["cost_usd"].is_null());
        if !shaped || line["outcome"] != outcome || line["gated"] != gated {
            return Err(format!("{nonce}: expected relayed {outcome} gated {gated}, saw {line}"));
        }
        Ok(line)
    }
}

fn listing(nonce: &str) -> Value {
    json!({ "ev": "relay_step", "nonce": nonce, "step": "listing" })
}

fn handing(nonce: &str, to: &str) -> Value {
    json!({ "ev": "relay_step", "nonce": nonce, "step": "handing", "to": to })
}

fn not_sent(why: &str) -> Value {
    json!({ "kind": "not_sent", "why": why, "detail": "" })
}

fn the_approved_send() -> Value {
    json!({ "tool": "SendMessage", "input": {
        "to": TARGET_ADDRESS, "message": BODY, "summary": SUMMARY,
        "type": "message", "recipient": TARGET_ADDRESS, "recipient_kind": "name", "content": BODY,
    } })
}

fn a_faithful_courier_delivers_once() -> Result<String, String> {
    with_courier("faithful", "faithful", None, &[], |client, relay| {
        let sent = Instant::now();
        client.relay("r-1", TARGET, BODY)?;
        client.expect_relayed(
            "r-1",
            &[listing("r-1"), handing("r-1", TARGET_ADDRESS)],
            json!({ "kind": "delivered", "to": TARGET_ADDRESS, "msg_id": "stub-msg-2" }),
            true,
        )?;
        let took = sent.elapsed();
        if relay.sent() != [the_approved_send()] {
            return Err(format!("exactly the approved send should have gone, once: {:?}", relay.sent()));
        }
        client.shutdown()?;
        Ok(format!("stub_relay={:.0}ms", ms(took)))
    })
}

/// A courier that does `behaviour` instead of copying the send. Nothing may go.
fn a_send_that_is_not_the_approved_one_goes_nowhere(name: &str, behaviour: &'static str) -> Result<String, String> {
    with_courier(name, behaviour, None, &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        let (before, line) = client.until_relayed("r-1")?;
        let refused = line["outcome"]["detail"].as_str().is_some_and(|d| d.contains("this is not the approved message"));
        if before != [listing("r-1"), handing("r-1", TARGET_ADDRESS)] || line["outcome"]["why"] != "model_did_not_call" || !refused || line["gated"] != true {
            return Err(format!("{name}: expected not_sent model_did_not_call naming the gate's refusal, saw {before:?} then {line}"));
        }
        if !relay.sent().is_empty() {
            return Err(format!("nothing should have been sent, the courier ran {:?}", relay.sent()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_courier_that_rewrites_the_text_sends_nothing() -> Result<String, String> {
    a_send_that_is_not_the_approved_one_goes_nowhere("paraphrase", "paraphrase")
}

fn a_message_that_says_send_it_elsewhere_goes_nowhere_else() -> Result<String, String> {
    a_send_that_is_not_the_approved_one_goes_nowhere("redirect", "redirect")
}

/// Two identical sends in one turn. Where the CLI ends the turn at the first result the second is
/// never made; where it does not run the stop hook (an older one) the second is made, and it is the
/// gate that refuses it. One send goes either way.
fn a_courier_that_calls_twice_sends_once() -> Result<String, String> {
    for behaviour in ["twice", "twice-nostop"] {
        with_courier(behaviour, behaviour, None, &[], |client, relay| {
            client.relay("r-1", TARGET, BODY)?;
            client.expect_relayed("r-1", &[listing("r-1"), handing("r-1", TARGET_ADDRESS)], json!({ "kind": "delivered", "to": TARGET_ADDRESS, "msg_id": "stub-msg-2" }), true)?;
            if relay.sent() != [the_approved_send()] {
                return Err(format!("{behaviour}: two identical calls should send once: {:?}", relay.sent()));
            }
            client.shutdown()?;
            Ok(String::new())
        })?;
    }
    Ok(String::new())
}

/// A CLI that does not run the stop hook ends each turn with the model's closing word instead. The
/// run is judged the same.
fn a_courier_whose_stop_hook_does_not_run_still_delivers() -> Result<String, String> {
    with_courier("nostop", "faithful-nostop", None, &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        client.expect_relayed("r-1", &[listing("r-1"), handing("r-1", TARGET_ADDRESS)], json!({ "kind": "delivered", "to": TARGET_ADDRESS, "msg_id": "stub-msg-2" }), true)?;
        if relay.sent() != [the_approved_send()] {
            return Err(format!("exactly the approved send should have gone, once: {:?}", relay.sent()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_courier_that_reaches_for_another_tool_gets_none() -> Result<String, String> {
    with_courier("bash", "bash", None, &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        client.expect_relayed("r-1", &[listing("r-1"), handing("r-1", TARGET_ADDRESS)], json!({ "kind": "delivered", "to": TARGET_ADDRESS, "msg_id": "stub-msg-3" }), true)?;
        if relay.sent() != [the_approved_send()] {
            return Err(format!("Bash should be refused and the approved send go once: {:?}", relay.sent()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

/// A title the listing cannot name one session for. The courier is never handed the send.
fn a_title_that_cannot_be_addressed(name: &str, title: &str, why: &str) -> Result<String, String> {
    with_courier(name, "faithful", None, &[], |client, relay| {
        client.relay("r-1", title, BODY)?;
        client.expect_relayed("r-1", &[listing("r-1")], not_sent(why), true)?;
        let told = relay.heard("courier");
        if told.len() != 1 || !told[0].starts_with("Call ListAgents") {
            return Err(format!("the courier should only have been asked for the listing, it was told {told:?}"));
        }
        if !relay.sent().is_empty() {
            return Err(format!("nothing should have been sent: {:?}", relay.sent()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_title_that_forges_a_line_reaches_nobody() -> Result<String, String> {
    a_title_that_cannot_be_addressed("forged", "冒名頂替", "not_found")
}

fn a_session_that_is_gone_is_not_found() -> Result<String, String> {
    a_title_that_cannot_be_addressed("gone", "已經不在的 session", "not_found")
}

fn two_sessions_with_one_title_are_ambiguous() -> Result<String, String> {
    a_title_that_cannot_be_addressed("twin", "雙胞胎", "ambiguous")
}

fn a_gate_that_does_not_run_stops_before_anything_can_be_sent() -> Result<String, String> {
    with_courier("nohook", "nohook", None, &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        let (before, line) = client.until_relayed("r-1")?;
        if before != [listing("r-1")] || line["outcome"]["kind"] != "not_sent" || line["outcome"]["why"] != "gate_absent" {
            return Err(format!("a listing the gate never saw should end the run as gate_absent before handing, saw {before:?} then {line}"));
        }
        if relay.heard("courier").len() != 1 || !relay.sent().is_empty() {
            return Err(format!("the courier should never have been handed the send: told {:?}, ran {:?}", relay.heard("courier"), relay.sent()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

/// Each outcome a run can come to when the courier dies or hangs, by where it stopped.
fn a_courier_that_dies_or_hangs_is_judged_by_where_it_stopped() -> Result<String, String> {
    let cases: [(&str, &'static str, Option<u64>, &str, &str); 5] = [
        ("die-listing", "die-listing", None, "not_sent", "exited (code 3)"),
        ("hang-listing", "hang-listing", Some(400), "not_sent", "did not finish its turn"),
        ("die-after-allow", "die-after-allow", None, "unknown", "no answer came back"),
        ("hang-after-allow", "hang-after-allow", Some(400), "unknown", "did not finish its turn"),
        // The gate never saw the send, so nothing is spent, and the send may still have gone.
        ("skipsend-die", "skipsend-die", None, "unknown", "no answer came back"),
    ];
    let mut took = Vec::new();
    for (name, behaviour, step_ms, kind, needle) in cases {
        with_courier(name, behaviour, step_ms, &[], |client, relay| {
            let began = Instant::now();
            client.relay("r-1", TARGET, BODY)?;
            let (_, line) = client.until_relayed("r-1")?;
            let detail = line["outcome"]["detail"].as_str().unwrap_or_default();
            if line["outcome"]["kind"] != kind || !detail.contains(needle) || line["gated"] != true {
                return Err(format!("{name}: expected {kind} saying {needle:?}, saw {line}"));
            }
            if kind == "not_sent" && line["outcome"]["why"] != "failed" {
                return Err(format!("{name}: a courier that failed before the gate let anything through is not_sent failed, saw {line}"));
            }
            if !relay.sent().is_empty() {
                return Err(format!("{name}: the courier should have run nothing: {:?}", relay.sent()));
            }
            took.push(format!("{name}={:.0}ms", ms(began.elapsed())));
            client.shutdown().map(drop)
        })?;
    }
    Ok(took.join(" "))
}

fn the_same_order_twice_is_delivered_once() -> Result<String, String> {
    with_courier("repeat", "faithful", None, &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        client.expect_relayed("r-1", &[listing("r-1"), handing("r-1", TARGET_ADDRESS)], json!({ "kind": "delivered", "to": TARGET_ADDRESS, "msg_id": "stub-msg-2" }), true)?;
        client.relay("r-1", TARGET, BODY)?;
        client.expect_relayed("r-1", &[], not_sent("repeated"), true)?;
        if relay.sent().len() != 1 {
            return Err(format!("one nonce should send once: {:?}", relay.sent()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn an_order_while_one_runs_is_busy() -> Result<String, String> {
    with_courier("busy", "hang-listing", Some(1500), &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        client.expect("the first relay starts", listing("r-1"))?;
        client.relay("r-2", TARGET, BODY)?;
        client.expect_relayed("r-2", &[], not_sent("busy"), true)?;
        // The order in flight sent again is a repeat, not a busy refusal a client could read as its end.
        client.relay("r-1", TARGET, BODY)?;
        client.expect_relayed("r-1", &[], not_sent("repeated"), true)?;
        let (_, first) = client.until_relayed("r-1")?;
        if first["outcome"]["why"] != "failed" {
            return Err(format!("the first relay should end on its own terms, saw {first}"));
        }
        if relay.heard("courier").len() != 1 {
            return Err(format!("the busy order should never reach a courier: {:?}", relay.heard("courier")));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_relay_ask_turn_has_no_utterance() -> Result<String, String> {
    with_courier("ask", "faithful", None, &[], |client, _| {
        let start = client.wait_for_start(1)?;
        let args: Vec<String> = start["args"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        let after = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
        if after("--json-schema").and_then(|s| serde_json::from_str::<Value>(&s).ok()) != Some(reply::schema(reply::Relaying::On)) {
            return Err(format!("with --relay the character should be offered the relay, its schema was {:?}", after("--json-schema")));
        }
        if after("--append-system-prompt") != Some(reply::prompt(&Character::standard(), reply::Relaying::On)) {
            return Err("with --relay the character should be told the relay rule".to_string());
        }
        client.say("relayask")?;
        client.expect("her relay ask", json!({ "ev": "relay_asked", "to": "AImate 那個 session", "text": "設定頁的主題做好了，可以看了。" }))?;
        client.expect("and the end of a turn that said nothing", json!({ "ev": "turn_end" }))?;
        client.say("badrelay")?;
        client.expect_error("a relay that breaks a limit refuses the turn", &["relay.text is empty"])?;
        client.say("ok")?;
        client.expect_turn("the next sentence is answered as ever", OK_TURN, json!(["nod", "offer"]))?;
        if let Some(line) = client.transcript.iter().find(|l| l.contains("已經幫你傳了")) {
            return Err(format!("what she claimed on the relay turn reached the wire: {line}"));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn relaying_is_off_unless_serve_is_told() -> Result<String, String> {
    with_stub("relay-off", |client| {
        let start = client.wait_for_start(1)?;
        let schema = start["args"].as_array().and_then(|a| a.iter().position(|v| v == "--json-schema").and_then(|i| a.get(i + 1))).and_then(Value::as_str).unwrap_or_default().to_string();
        if serde_json::from_str::<Value>(&schema).ok() != Some(reply::schema(reply::Relaying::Off)) {
            return Err(format!("without --relay the schema should be today's, it was {schema}"));
        }
        client.relay("r-1", TARGET, BODY)?;
        client.expect("a relay with relaying off", json!({ "ev": "relayed", "nonce": "r-1", "outcome": not_sent("off"), "gated": true, "elapsed_ms": 0, "cost_usd": null }))?;
        client.say("relayask")?;
        client.expect_turn("a relay key from the model is ignored, as before", "[nod]已經幫你傳了", json!(["nod"]))?;
        if client.starts().len() != 1 {
            return Err(format!("no courier should have started, the stand-in started {} times", client.starts().len()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn an_order_that_breaks_a_limit_is_still_answered() -> Result<String, String> {
    with_courier("unfit", "faithful", None, &[], |client, relay| {
        client.relay("r-1", TARGET, "")?;
        client.expect("an empty body", json!({ "ev": "relayed", "nonce": "r-1", "outcome": { "kind": "not_sent", "why": "failed", "detail": "body is empty" }, "gated": true, "elapsed_ms": 0, "cost_usd": null }))?;
        client.relay("R 1", TARGET, BODY)?;
        client.expect_error("a nonce that cannot be answered", &["bad request", "nonce"])?;
        if !relay.heard("courier").is_empty() {
            return Err("neither should reach a courier".to_string());
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn the_next_sentence_carries_the_last_outcome_once() -> Result<String, String> {
    with_courier("note", "faithful", None, &[], |client, relay| {
        client.say("ok")?;
        client.expect_turn("a sentence before any relay", OK_TURN, json!(["nod", "offer"]))?;
        client.relay("r-1", TARGET, BODY)?;
        client.until_relayed("r-1")?;
        for n in 0..2 {
            client.say("echo:hi")?;
            let (_, end) = (client.recv(&format!("say {n} after the relay"))?, client.recv("its turn_end")?);
            if end != json!({ "ev": "turn_end" }) {
                return Err(format!("say {n}: expected one utterance and turn_end, saw {end}"));
            }
        }
        let told = relay.heard("character");
        let want = vec!["ok".to_string(), format!("{}\necho:hi", reply::note(relay::Fact::Delivered)), "echo:hi".to_string()];
        if told != want {
            return Err(format!("the sentence after the relay should carry the note once, the character was told {told:?}"));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

/// A sentence the engine failed to answer may not have told her how the relay ended, so the next
/// one says it again, and the one after that does not.
fn an_outcome_outlives_a_sentence_that_was_not_answered() -> Result<String, String> {
    with_courier("note_kept", "faithful", None, &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        client.until_relayed("r-1")?;
        client.say("notjson")?;
        client.expect_error("the sentence that carried the note is not answered", &["not JSON"])?;
        for n in 0..2 {
            client.say("ok")?;
            client.expect_turn(&format!("say {n} after the failed one"), OK_TURN, json!(["nod", "offer"]))?;
        }
        let note = reply::note(relay::Fact::Delivered);
        let told = relay.heard("character");
        let want = vec![format!("{note}\nnotjson"), format!("{note}\nok"), "ok".to_string()];
        if told != want {
            return Err(format!("the note should stay until a sentence is answered, the character was told {told:?}"));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn the_courier_runs_without_the_speech_and_chat_keys() -> Result<String, String> {
    let env = [(voice::KEY_ENV_ENV, KEY_VAR), (KEY_VAR, KEY), (chat::KEY_ENV_ENV, "VRCHATCORE_SELFTEST_CHAT_KEY"), ("VRCHATCORE_SELFTEST_CHAT_KEY", KEY)];
    with_courier("keys", "faithful", None, &env, |client, _| {
        client.relay("r-1", TARGET, BODY)?;
        client.until_relayed("r-1")?;
        let courier = client.starts().into_iter().find(|s| s["args"].as_array().is_some_and(|a| a.iter().any(|v| v == relay::TOOLS))).ok_or("the courier never started")?;
        if courier["sees_key"] != false || courier["sees_chat_key"] != false {
            return Err(format!("the courier should be started without the speech and chat keys, it saw {} and {}", courier["sees_key"], courier["sees_chat_key"]));
        }
        client.shutdown()?;
        client.never_said(KEY)
    })?;
    Ok(String::new())
}

fn the_courier_is_started_with_the_pinned_flags_intact() -> Result<String, String> {
    with_courier("flags", "faithful", None, &[], |client, _| {
        client.relay("r-1", TARGET, BODY)?;
        client.until_relayed("r-1")?;
        let courier = client.starts().into_iter().find(|s| s["args"].as_array().is_some_and(|a| a.iter().any(|v| v == relay::TOOLS))).ok_or("the courier never started")?;
        let args: Vec<String> = courier["args"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if args[..checks::COURIER_FLAGS.len().min(args.len())] != checks::COURIER_FLAGS {
            return Err(format!("the courier's arguments should start with {:?}, they are {args:?}", checks::COURIER_FLAGS));
        }
        let tail: Vec<&str> = args.iter().rev().take(checks::ISOLATION.len()).rev().map(String::as_str).collect();
        if tail != checks::ISOLATION {
            return Err(format!("the courier's arguments should end with {:?}, they end with {tail:?}", checks::ISOLATION));
        }
        let settings: Value = args.iter().position(|a| a == "--settings").and_then(|i| args.get(i + 1)).and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
        let command = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap_or_default();
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        if !command.starts_with(&format!("\"{exe}\" relay-gate \"")) || !command.contains("/relay-r-1/gate\"") {
            return Err(format!("the hook should run this executable on the run's gate folder, it is {command:?}"));
        }
        let after = &settings["hooks"]["PostToolUse"][0];
        if after["hooks"][0]["command"] != format!("\"{exe}\" relay-stop") || after["matcher"] != relay::MATCHER {
            return Err(format!("the stop should run this executable's relay-stop on the same tools, it is {after}"));
        }
        let cwd = cwd_of(&courier)?;
        if cwd.file_name().and_then(|n| n.to_str()) != Some(relay::COURIER_DIR) || courier["entries"] != json!(0) {
            return Err(format!("the courier should run in an empty folder named {}, it ran in {} holding {}", relay::COURIER_DIR, cwd.display(), courier["entries"]));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

fn the_courier_and_its_folder_are_gone_after_shutdown() -> Result<String, String> {
    with_courier("cleanup", "hang-listing", None, &[], |client, _| {
        client.relay("r-1", TARGET, BODY)?;
        client.expect("the relay starts", listing("r-1"))?;
        let deadline = Instant::now() + STEP_TIMEOUT;
        let courier = loop {
            if let Some(start) = client.starts().into_iter().find(|s| s["args"].as_array().is_some_and(|a| a.iter().any(|v| v == relay::TOOLS))) {
                break start;
            }
            if Instant::now() >= deadline {
                return Err("the courier never started".to_string());
            }
            thread::sleep(Duration::from_millis(10));
        };
        let (pid, cwd) = (pid_of(&courier)?, cwd_of(&courier)?);
        let took = client.shutdown()?;
        wait_gone(pid).map_err(|e| format!("the courier outlived the core: {e}"))?;
        let run = cwd.parent().map(Path::to_path_buf).unwrap_or_default();
        let root = run.parent().map(Path::to_path_buf).unwrap_or_default();
        if run.exists() || root.exists() {
            return Err(format!("the relay's folder {} was left behind", run.display()));
        }
        Ok(format!("leave_mid_relay={:.0}ms", ms(took)))
    })
}

fn she_answers_while_a_relay_runs() -> Result<String, String> {
    with_courier("talk", "hang-listing", Some(3000), &[], |client, _| {
        client.relay("r-1", TARGET, BODY)?;
        client.expect("the relay starts", listing("r-1"))?;
        client.say("ok")?;
        client.expect_turn("a sentence while the courier runs is answered first", OK_TURN, json!(["nod", "offer"]))?;
        client.until_relayed("r-1")?;
        client.shutdown()?;
        Ok(String::new())
    })
}

fn a_send_the_gate_never_saw_is_reported_as_it_was() -> Result<String, String> {
    with_courier("ungated", "skipsend", None, &[], |client, _| {
        client.relay("r-1", TARGET, BODY)?;
        client.expect_relayed("r-1", &[listing("r-1"), handing("r-1", TARGET_ADDRESS)], json!({ "kind": "delivered", "to": TARGET_ADDRESS, "msg_id": "stub-msg-2" }), false)?;
        client.shutdown().map(drop)
    })?;
    with_courier("leaked", "skipsend-paraphrase", None, &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        let message = format!("{BODY}（我幫你潤飾了一下）");
        client.expect_relayed("r-1", &[listing("r-1"), handing("r-1", TARGET_ADDRESS)], json!({ "kind": "sent_other", "to": TARGET_ADDRESS, "message": message }), false)?;
        if relay.sent().len() != 1 {
            return Err(format!("the stand-in sent one message past a gate that did not run: {:?}", relay.sent()));
        }
        client.shutdown().map(drop)
    })?;
    Ok(String::new())
}

/// `relay-gate` as the CLI runs it, one process per call. Four processes handed the approved send at
/// once let exactly one through, and a gate that cannot read its call denies, with exit code 0.
fn the_gate_process_lets_one_of_four_racing_sends_through() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("locating own executable: {e}"))?;
    let approved = json!({ "to": TARGET_ADDRESS, "message": BODY, "summary": SUMMARY });
    let call = json!({ "hook_event_name": "PreToolUse", "tool_name": "SendMessage", "tool_input": the_approved_send()["input"] }).to_string();
    let gate = |dir: &Path, stdin: &[u8], args: &[&str]| -> Result<(Value, Option<i32>), String> {
        let out = Command::new(&exe).arg("relay-gate").args(args).arg(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
            .and_then(|mut child| {
                if let Some(mut pipe) = child.stdin.take() {
                    let _ = pipe.write_all(stdin);
                }
                child.wait_with_output()
            })
            .map_err(|e| format!("running relay-gate: {e}"))?;
        let decision = serde_json::from_slice::<Value>(&out.stdout).map_err(|e| format!("relay-gate printed {:?} ({e})", String::from_utf8_lossy(&out.stdout)))?;
        Ok((decision["hookSpecificOutput"].clone(), out.status.code()))
    };
    let mut allowed = Vec::new();
    for round in 0..5 {
        let dir = std::env::temp_dir().join(format!("vrchatcore-selftest-{}-gate-procs-{round}", std::process::id()));
        scratch::fresh(&dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
        std::fs::write(dir.join("armed.json"), approved.to_string()).map_err(|e| format!("arming: {e}"))?;
        let mut children = Vec::new();
        for _ in 0..4 {
            children.push(Command::new(&exe).arg("relay-gate").arg(&dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("running relay-gate: {e}"))?);
        }
        for child in &mut children {
            if let Some(mut pipe) = child.stdin.take() {
                let _ = pipe.write_all(call.as_bytes());
            }
        }
        let mut allows = 0;
        for child in children {
            let out = child.wait_with_output().map_err(|e| format!("waiting for relay-gate: {e}"))?;
            let decision: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
            if out.status.code() != Some(0) {
                return Err(format!("relay-gate should exit 0 for every answer, it exited {:?}", out.status.code()));
            }
            allows += usize::from(decision["hookSpecificOutput"]["permissionDecision"] == "allow");
        }
        scratch::remove(&dir);
        allowed.push(allows);
    }
    if allowed.iter().any(|&n| n != 1) {
        return Err(format!("four racing gates should allow one send each round, they allowed {allowed:?}"));
    }
    let dir = std::env::temp_dir().join(format!("vrchatcore-selftest-{}-gate-procs-bad", std::process::id()));
    scratch::fresh(&dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
    for (stdin, args, what) in [(&b"not json"[..], &[][..], "a call it cannot read"), (call.as_bytes(), &["extra"][..], "a second folder"), (call.as_bytes(), &[][..], "a send with nothing armed")] {
        let (said, code) = gate(&dir, stdin, args)?;
        if said["permissionDecision"] != "deny" || code != Some(0) {
            return Err(format!("{what} should be a deny with exit code 0, it was {said} with {code:?}"));
        }
    }
    scratch::remove(&dir);
    Ok(String::new())
}

/// `relay-stop` as the CLI runs it: one line, exit code 0, whatever it is handed.
fn the_stop_hook_process_prints_the_stop_line() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("locating own executable: {e}"))?;
    let call = json!({ "hook_event_name": "PostToolUse", "tool_name": "ListAgents", "tool_input": {}, "tool_response": {} }).to_string();
    let out = Command::new(&exe).arg("relay-stop").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
        .and_then(|mut child| {
            if let Some(mut pipe) = child.stdin.take() {
                let _ = pipe.write_all(call.as_bytes());
            }
            child.wait_with_output()
        })
        .map_err(|e| format!("running relay-stop: {e}"))?;
    if out.stdout != b"{\"continue\":false,\"stopReason\":\"relay: the call is done\"}\n" || out.status.code() != Some(0) {
        return Err(format!("relay-stop should print the stop line and exit 0, it printed {:?} and exited {:?}", String::from_utf8_lossy(&out.stdout), out.status.code()));
    }
    Ok(String::new())
}

/// The stand-in courier as the CLI behaves with a `PostToolUse` hook (measured 2026-11-13): after a
/// call that ran, a hook that prints `continue: false` ends the turn with no closing word from the
/// model, and a hook that cannot be started, or prints anything else, changes nothing. The courier
/// checks above run on it, so it is checked on its own stream here.
fn the_stand_in_courier_ends_a_turn_where_its_stop_hook_says() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("locating own executable: {e}"))?;
    let path = exe.to_string_lossy().replace('\\', "/");
    let stream = |behaviour: &str, hook: &str| -> Result<Vec<String>, String> {
        let settings = json!({ "hooks": { "PostToolUse": [{ "matcher": relay::MATCHER, "hooks": [{ "type": "command", "command": hook }] }] } });
        let out = Command::new(&exe).arg("-p").args(["--model", relay::MODEL, "--tools", relay::TOOLS, "--settings", &settings.to_string()])
            .env(stub::COURIER_ENV, behaviour).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
            .and_then(|mut child| {
                if let Some(mut pipe) = child.stdin.take() {
                    let _ = pipe.write_all(claude::user_line(relay::list_line()).as_bytes());
                }
                child.wait_with_output()
            })
            .map_err(|e| format!("running the stand-in courier: {e}"))?;
        let lines = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect::<Vec<_>>();
        Ok(lines.iter().map(|l| (if l["message"]["content"][0]["text"] == "DONE" { "DONE" } else { l["type"].as_str().unwrap_or("?") }).to_string()).collect())
    };
    let stop = format!("\"{path}\" relay-stop");
    let ended = ["system", "assistant", "user", "result"];
    let closed = ["system", "assistant", "user", "DONE", "result"];
    let cases: [(&str, &str, String, &[&str]); 4] = [
        ("a hook that prints continue: false", "faithful", stop.clone(), &ended),
        ("a CLI that does not run the hook", "faithful-nostop", stop, &closed),
        ("a hook that cannot be started", "faithful", format!("\"{path}.missing\" relay-stop"), &closed),
        ("a hook that prints something else", "faithful", format!("\"{path}\" relay-gate"), &closed),
    ];
    for (what, behaviour, hook, want) in cases {
        let got = stream(behaviour, &hook)?;
        if got != want {
            return Err(format!("{what}: the listing turn should print {want:?}, it printed {got:?}"));
        }
    }
    Ok(String::new())
}

/// Neither the character's `claude` nor the courier is started with `CLAUDE_CODE_ENTRYPOINT`, even
/// when the core has it, and both keep the other `CLAUDE_CODE_` variables.
fn claude_children_do_not_inherit_the_hosts_entrypoint() -> Result<String, String> {
    let env = [("CLAUDE_CODE_ENTRYPOINT", "claude-desktop"), ("CLAUDE_CODE_SELFTEST_KEPT", "1")];
    with_courier("entrypoint", "faithful", None, &env, |client, _| {
        client.relay("r-1", TARGET, BODY)?;
        client.until_relayed("r-1")?;
        let starts = client.starts();
        let is_courier = |start: &Value| start["args"].as_array().is_some_and(|a| a.iter().any(|v| v == relay::TOOLS));
        if !starts.iter().any(is_courier) || starts.iter().all(is_courier) {
            return Err(format!("both the character's claude and the courier should have started, the stand-in started {} times", starts.len()));
        }
        for start in &starts {
            let who = if is_courier(start) { "the courier" } else { "the character's claude" };
            let names: Vec<&str> = start["claude_env"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            if names.contains(&"CLAUDE_CODE_ENTRYPOINT") {
                return Err(format!("{who} should be started without CLAUDE_CODE_ENTRYPOINT, the stand-in saw {names:?}"));
            }
            if !names.contains(&"CLAUDE_CODE_SELFTEST_KEPT") {
                return Err(format!("{who} should keep the other CLAUDE_CODE_ variables, the stand-in saw {names:?}"));
            }
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

/// The outcome goes out while the courier's process and the run's folder are still there, and
/// `elapsed_ms` stops where the outcome does. The stand-in takes 1.5 s to leave once its input
/// closes, so the process outlasts the 400 ms it is given and has to be killed.
fn the_outcome_goes_out_before_the_courier_and_its_folder_are_gone() -> Result<String, String> {
    with_courier("linger", "linger", None, &[], |client, _| {
        let sent = Instant::now();
        client.relay("r-1", TARGET, BODY)?;
        let line = client.expect_relayed("r-1", &[listing("r-1"), handing("r-1", TARGET_ADDRESS)], json!({ "kind": "delivered", "to": TARGET_ADDRESS, "msg_id": "stub-msg-2" }), true)?;
        let courier = client.starts().into_iter().find(|s| s["args"].as_array().is_some_and(|a| a.iter().any(|v| v == relay::TOOLS))).ok_or("the courier never started")?;
        let (pid, cwd) = (pid_of(&courier)?, cwd_of(&courier)?);
        let run = cwd.parent().map(Path::to_path_buf).unwrap_or_default();
        if !run.exists() {
            return Err(format!("the run's folder {} was gone before the outcome went out", run.display()));
        }
        wait_gone(pid).map_err(|e| format!("the courier was never ended: {e}"))?;
        let deadline = Instant::now() + STEP_TIMEOUT;
        while run.exists() {
            if Instant::now() >= deadline {
                return Err(format!("the run's folder {} was left behind", run.display()));
            }
            thread::sleep(Duration::from_millis(10));
        }
        let (total, elapsed) = (ms(sent.elapsed()), line["elapsed_ms"].as_f64().unwrap_or_default());
        if elapsed + 300.0 > total {
            return Err(format!("elapsed_ms ({elapsed:.0}) should stop where the outcome goes out, and the courier's 400 ms of leaving come after it, but the whole took {total:.0} ms"));
        }
        client.shutdown()?;
        Ok(format!("outcome_before_cleanup={elapsed:.0}ms_of_{total:.0}ms"))
    })
}

/// A courier that has not made its call when its turn times out may still make it. It is stopped
/// before the outcome is read, so that a `not_sent` is not followed by a send. The stand-in takes
/// 1.6 s to make the call and each turn is given 1 s, which the listing turn needs a small part of.
fn a_courier_that_times_out_cannot_send_after_the_outcome() -> Result<String, String> {
    with_courier("late", "late-send", Some(1000), &[], |client, relay| {
        client.relay("r-1", TARGET, BODY)?;
        let (before, line) = client.until_relayed("r-1")?;
        if before != [listing("r-1"), handing("r-1", TARGET_ADDRESS)] || line["outcome"]["kind"] != "not_sent" || line["outcome"]["why"] != "failed" {
            return Err(format!("the send turn should time out as not_sent failed, saw {before:?} then {line}"));
        }
        thread::sleep(Duration::from_millis(900));
        if !relay.sent().is_empty() {
            return Err(format!("the courier sent after the outcome said nothing had gone: {:?}", relay.sent()));
        }
        client.shutdown()?;
        Ok(String::new())
    })
}

// The dialogue record (Docs/DESIGN_ChatCore.md §13). The core is told where on each `say`, so these
// stand in for the pet by naming the file the pet would name, in a home of their own.

/// A folder under the temp folder, empty at the start and removed when dropped.
struct Gone(PathBuf);

impl Gone {
    fn fresh(name: &str) -> Result<Gone, String> {
        let dir = std::env::temp_dir().join(format!("vrchatcore-selftest-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
        Ok(Gone(dir))
    }
}

impl Drop for Gone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Client {
    fn say_kept(&mut self, text: &str, to: &Path, via: &str) -> Result<(), String> {
        self.send(json!({ "op": "say", "text": text, "record": { "to": to, "via": via } }).to_string().as_bytes())
    }
}

fn kept_lines(file: &Path) -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("reading {}: {e}", file.display()))?;
    text.lines()
        .enumerate()
        .map(|(n, line)| serde_json::from_str(line).map_err(|e| format!("line {} of {} is not JSON ({e}): {line}", n + 1, file.display())))
        .collect()
}

fn names_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).map(|it| it.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

const KEPT_KEYS: [&str; 10] = ["v", "at", "run", "turn", "user", "reply", "outcome", "engine", "ms", "gap_ms"];

/// The line against `want` in every field but the four that vary from run to run, and with no field
/// missing or added.
fn kept_as(line: &Value, want: Value, what: &str) -> Result<(), String> {
    let keys: Vec<&str> = line.as_object().map(|o| o.keys().map(String::as_str).collect()).unwrap_or_default();
    if keys.len() != KEPT_KEYS.len() || !KEPT_KEYS.iter().all(|k| keys.contains(k)) {
        return Err(format!("{what}: the line should have exactly {KEPT_KEYS:?}, it has {keys:?}: {line}"));
    }
    let mut fixed = line.clone();
    if let Some(object) = fixed.as_object_mut() {
        for key in ["at", "run", "ms", "gap_ms"] {
            object.remove(key);
        }
    }
    if fixed == want { Ok(()) } else { Err(format!("{what}: expected {want} besides at, run, ms and gap_ms, the line is {line}")) }
}

fn ms_keys(line: &Value) -> Vec<String> {
    let mut keys: Vec<String> = line["ms"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    keys.sort();
    keys
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Off, a turn and a failed one write nothing anywhere. On, each kept turn is one line with the
/// user's words and her clauses as said, a refused or failed turn has its outcome and no reply, and
/// a `say` without `record` after them adds nothing. The only file anywhere is the one named, in the
/// home, not the profile or the cwd.
fn a_turn_is_kept_only_when_its_say_asks_and_only_where_it_says() -> Result<String, String> {
    let root = Gone::fresh("record")?;
    let (home, profile) = (root.0.join("home"), root.0.join("profile"));
    for dir in [&home, &profile] {
        std::fs::create_dir_all(dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
    }
    let file = home.join("dialogue.jsonl");
    let cwd = std::env::current_dir().map(|d| d.join("dialogue.jsonl")).unwrap_or_default();
    let cwd_had = cwd.exists();
    let env = [("VRPET_HOME", home.to_str().unwrap_or_default()), ("USERPROFILE", profile.to_str().unwrap_or_default())];
    let audio_ms = FakeSpeech::voice()["samples"].as_f64().map(|s| (s * 1000.0 / f64::from(voice::SAMPLE_RATE)).round()).unwrap_or(-1.0);
    let started = epoch_now();
    with_core_configured(Mode::Stub { log: root.0.join("stub.log"), model: None }, &FakeSpeech::get()?.url, &env, |client| {
        client.expect_ready()?;
        client.say("ok")?;
        client.expect_turn("a turn whose say does not ask", OK_TURN, json!(["nod", "offer"]))?;
        client.say("error")?;
        client.expect_error("a failed turn whose say does not ask", &["the stub failed on purpose"])?;
        // The next job, so once it has ended the two turns before it are done, writes and all.
        client.greet("嗨")?;
        client.expect_turn("a greeting", "嗨", json!([]))?;
        let written: Vec<String> = [&home, &profile].into_iter().flat_map(|d| names_in(d)).collect();
        if !written.is_empty() {
            return Err(format!("turns whose say did not ask for a record wrote {written:?}"));
        }
        client.say_kept("ok", &file, "mic")?;
        client.expect_turn("a kept turn is answered as ever", OK_TURN, json!(["nod", "offer"]))?;
        client.say_kept("bracket", &file, "text")?;
        client.expect_error("a kept turn the core refuses", &["contains"])?;
        client.say_kept("error", &file, "text")?;
        client.expect_error("a kept turn that failed", &["the stub failed on purpose"])?;
        client.say("ok")?;
        client.expect_turn("a turn whose say no longer asks", OK_TURN, json!(["nod", "offer"]))?;
        client.shutdown().map(drop)
    })?;
    let finished = epoch_now();

    let lines = kept_lines(&file)?;
    if lines.len() != 3 {
        return Err(format!("three turns asked to be kept, the file has {} lines: {lines:?}", lines.len()));
    }
    let engine = json!({ "kind": "claude", "model": "haiku" });
    let ok_reply = json!({ "clauses": ["收到", "我在這裡"], "language": "zh-TW", "affect": "calm" });
    let user = |text: &str, via: &str| json!({ "text": text, "via": via });
    let turn = |n: u64, said: Value, reply: Value, outcome: &str| json!({ "v": 1, "turn": n, "user": said, "reply": reply, "outcome": outcome, "engine": engine });
    kept_as(&lines[0], turn(3, user("ok", "mic"), ok_reply, "answered"), "the kept turn, the third of this core")?;
    kept_as(&lines[1], turn(4, user("bracket", "text"), Value::Null, "refused"), "the refused turn")?;
    kept_as(&lines[2], turn(5, user("error", "text"), Value::Null, "failed"), "the failed turn")?;

    let run = lines[0]["run"].as_str().unwrap_or_default();
    if run.is_empty() || lines.iter().any(|l| l["run"] != run) {
        return Err(format!("every line of one core should carry one run, saw {:?}", lines.iter().map(|l| l["run"].clone()).collect::<Vec<_>>()));
    }
    if let Some(line) = lines.iter().find(|l| l["at"].as_u64().is_none_or(|at| at < started || at > finished)) {
        return Err(format!("at should be the epoch milliseconds the sentence arrived, between {started} and {finished}: {line}"));
    }
    let shapes: Vec<Vec<String>> = lines.iter().map(ms_keys).collect();
    let want: Vec<Vec<String>> = [&["audio", "done", "engine", "first_audio"][..], &["done", "engine"], &["done", "engine"]]
        .iter()
        .map(|keys| keys.iter().map(|k| k.to_string()).collect())
        .collect();
    if shapes != want {
        return Err(format!("ms should hold what each turn got to, {want:?}, it holds {shapes:?}"));
    }
    let ms = |key: &str| lines[0]["ms"][key].as_f64().unwrap_or(-1.0);
    if !(0.0 <= ms("engine") && ms("engine") <= ms("first_audio") && ms("first_audio") <= ms("done")) || ms("audio") != audio_ms {
        return Err(format!("the answered turn's ms should run engine <= first_audio <= done with {audio_ms} ms of audio: {}", lines[0]["ms"]));
    }
    if !lines[0]["gap_ms"].as_i64().is_some_and(|gap| gap >= 0) {
        return Err(format!("gap_ms should count from the greeting that ended before it: {}", lines[0]));
    }

    let (in_home, in_profile) = (names_in(&home), names_in(&profile));
    if in_home != ["dialogue.jsonl"] || !in_profile.is_empty() || (!cwd_had && cwd.exists()) {
        return Err(format!("the named file should be the only one written: the home has {in_home:?}, the profile {in_profile:?}, the cwd's dialogue.jsonl exists: {}", cwd.exists()));
    }
    Ok(format!("kept_line={}", std::fs::read_to_string(&file).unwrap_or_default().lines().next().unwrap_or_default()))
}

/// A folder that has gone, and on Windows a file another process holds, cost the turn nothing: she
/// answers each one, and stderr, which the pet keeps as its log, says so once for each outage and
/// not once a turn. A write that works again in between ends the first outage.
fn a_record_that_cannot_be_written_costs_the_turn_nothing() -> Result<String, String> {
    let root = Gone::fresh("record-fail")?;
    let missing = root.0.join("gone").join("dialogue.jsonl");
    let locked = root.0.join("locked.jsonl");
    let mut answered = Vec::new();
    let stderr = with_core(Mode::Echo { warmup_ms: None }, |client| {
        client.expect_ready()?;
        for n in 1..=2 {
            let text = format!("folder gone {n}");
            let sent = Instant::now();
            client.say_kept(&text, &missing, "text")?;
            client.expect_turn(&format!("turn {n} into a folder that is not there"), &text, json!([]))?;
            answered.push(ms(sent.elapsed()));
        }
        let folder = missing.parent().unwrap_or(&root.0);
        std::fs::create_dir_all(folder).map_err(|e| format!("making {}: {e}", folder.display()))?;
        client.say_kept("folder back", &missing, "text")?;
        client.expect_turn("the turn once the folder is back", "folder back", json!([]))?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let held = std::fs::OpenOptions::new().write(true).create(true).share_mode(0).open(&locked).map_err(|e| format!("holding {}: {e}", locked.display()))?;
            for n in 1..=2 {
                let text = format!("file held {n}");
                let sent = Instant::now();
                client.say_kept(&text, &locked, "text")?;
                client.expect_turn(&format!("turn {n} into a file another process holds"), &text, json!([]))?;
                answered.push(ms(sent.elapsed()));
            }
            client.greet("嗨")?;
            client.expect_turn("a greeting, so the last write was tried before the file is let go", "嗨", json!([]))?;
            drop(held);
        }
        Ok(client.stop())
    })?;
    let notes = stderr.lines().filter(|l| l.contains("the dialogue record could not be written")).count();
    let outages = if cfg!(windows) { 2 } else { 1 };
    if notes != outages {
        return Err(format!("{outages} outages should be noted {outages} times on stderr, it was {notes}: {stderr}"));
    }
    let lines = kept_lines(&missing)?;
    if lines.len() != 1 || lines[0]["user"]["text"] != "folder back" {
        return Err(format!("only the turn after the folder came back should be in it: {lines:?}"));
    }
    if std::fs::metadata(&locked).map(|m| m.len()).unwrap_or(0) != 0 {
        return Err("the held file was written to".to_string());
    }
    Ok(format!("unwritable_record_turns={:.1}ms_max", answered.iter().copied().fold(0.0, f64::max)))
}

/// What she asked to pass on and where is the relay's, never the record's: her relay turn is kept
/// with its outcome and no reply. The note that tells her how the relay ended rides in front of the
/// next sentence, and the record keeps the sentence as the user said it.
fn a_relay_turn_is_kept_without_what_she_asked_to_pass_on() -> Result<String, String> {
    let root = Gone::fresh("record-relay")?;
    let file = root.0.join("dialogue.jsonl");
    with_courier("record", "faithful", None, &[], |client, relay| {
        client.say_kept("relayask", &file, "mic")?;
        client.expect("her relay ask", json!({ "ev": "relay_asked", "to": "AImate 那個 session", "text": "設定頁的主題做好了，可以看了。" }))?;
        client.expect("and the end of a turn that said nothing", json!({ "ev": "turn_end" }))?;
        client.relay("r-1", TARGET, BODY)?;
        client.until_relayed("r-1")?;
        client.say_kept("ok", &file, "text")?;
        client.expect_turn("the sentence after the relay", OK_TURN, json!(["nod", "offer"]))?;
        if relay.heard("character").last().is_none_or(|told| !told.starts_with(reply::note(relay::Fact::Delivered))) {
            return Err(format!("the character should have been told how the relay ended, she was told {:?}", relay.heard("character")));
        }
        client.shutdown().map(drop)
    })?;
    let lines = kept_lines(&file)?;
    if lines.len() != 2 {
        return Err(format!("two kept turns, the file has {} lines: {lines:?}", lines.len()));
    }
    let engine = json!({ "kind": "claude", "model": "haiku" });
    kept_as(&lines[0], json!({ "v": 1, "turn": 1, "user": { "text": "relayask", "via": "mic" }, "reply": null, "outcome": "relay_asked", "engine": engine }), "her relay turn")?;
    let reply = json!({ "clauses": ["收到", "我在這裡"], "language": "zh-TW", "affect": "calm" });
    kept_as(&lines[1], json!({ "v": 1, "turn": 2, "user": { "text": "ok", "via": "text" }, "reply": reply, "outcome": "answered", "engine": engine }), "the sentence after it, without the note")?;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    for needle in ["設定頁的主題做好了", "AImate 那個 session", "已經幫你傳了", TARGET, "桌寵告訴你的"] {
        if text.contains(needle) {
            return Err(format!("the record holds {needle:?}, which is the relay's: {text}"));
        }
    }
    Ok(String::new())
}

/// A turn the core is asked to leave in, once the stand-in has the sentence, is kept as
/// `interrupted` with no reply and only the time it took to end. The stand-in never answers `hang`.
fn a_turn_the_core_is_stopped_in_is_kept_as_interrupted() -> Result<String, String> {
    let root = Gone::fresh("record-stop")?;
    let file = root.0.join("dialogue.jsonl");
    with_courier("record-stop", "faithful", None, &[], |client, relay| {
        client.say_kept("hang", &file, "text")?;
        let deadline = Instant::now() + STEP_TIMEOUT;
        while !relay.heard("character").iter().any(|told| told == "hang") {
            if Instant::now() >= deadline {
                return Err(format!("the stand-in never got the sentence, it was told {:?}", relay.heard("character")));
            }
            thread::sleep(Duration::from_millis(10));
        }
        client.shutdown().map(drop)
    })?;
    let lines = kept_lines(&file)?;
    if lines.len() != 1 {
        return Err(format!("one kept turn, the file has {} lines: {lines:?}", lines.len()));
    }
    let engine = json!({ "kind": "claude", "model": "haiku" });
    kept_as(&lines[0], json!({ "v": 1, "turn": 1, "user": { "text": "hang", "via": "text" }, "reply": null, "outcome": "interrupted", "engine": engine }), "the turn the core was stopped in")?;
    if ms_keys(&lines[0]) != ["done"] || lines[0]["gap_ms"] != Value::Null {
        return Err(format!("a stopped turn has only done in ms, and the first turn of a core has no gap: {}", lines[0]));
    }
    Ok(String::new())
}

pub fn run() -> ExitCode {
    let mut ran: Vec<&str> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut numbers: Vec<String> = Vec::new();

    for (name, verdict) in checks::run_all() {
        ran.push(name);
        if let Err(why) = verdict {
            failed.push(format!("{name}: {why}"));
        }
    }
    for &(name, scenario) in SCENARIOS {
        ran.push(name);
        match scenario() {
            Ok(note) if note.is_empty() => {}
            Ok(note) => numbers.push(note),
            Err(why) => failed.push(format!("{name}: {why}")),
        }
    }
    report(&ran, &numbers, &failed)
}

fn report(ran: &[&str], numbers: &[String], failed: &[String]) -> ExitCode {
    if failed.is_empty() {
        println!("SelfTest: PASS vrchatcore: {} | {} checks: {}", numbers.join(" "), ran.len(), ran.join(", "));
        ExitCode::SUCCESS
    } else {
        let all = failed.join(" || ");
        println!("SelfTest: FAIL vrchatcore: {} of {} failed: {}", failed.len(), ran.len(), all.replace(['\r', '\n'], " | "));
        ExitCode::FAILURE
    }
}
