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
//
// The transport check deliberately does not use `protocol`: it checks the wire against literal
// JSON, not the structs that wrote it.
//
// Every failure is a named Err that says which assertion broke and what was seen, because a
// self-test that can only say "failed" costs a debugging session.

use crate::character::{Character, DEFAULT_VOICE};
use crate::checks;
use crate::claude;
use crate::reply;
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
}

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
        for name in [voice::MODEL_ENV, voice::VOICE_ENV, voice::KEY_ENV_ENV] {
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
        if schema != Some(reply::schema()) {
            return Err(format!("--json-schema arrived as {:?}, not as the schema", after("--json-schema")));
        }
        if after("--append-system-prompt") != Some(&reply::prompt(&Character::standard())) {
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
        // Four one-syllable clauses. Three play 0.63 s and the fourth takes 0.38 s, so the pace sends
        // the fourth as a part of its own; each part lists only its own tags.
        client.expect_parts(
            "four clauses, a gesture used twice and one with none",
            &[("[nod]一 二 [nod]三", json!(["nod"])), ("[think]四", json!(["think"]))],
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
        if prompt_of(&first) != Some(reply::prompt(&Character::standard())) {
            return Err(format!("before any character is sent she is 小克, the stand-in was started with {:?}", prompt_of(&first)));
        }

        // Lines, quotes, a backslash and an emoji: the persona crosses a Windows command line.
        let persona = "第一行\n第二行 \"引號\" \\ 反斜線 😀";
        let flower = Character::new("阿花", persona, None)?;
        client.character("阿花", persona, None)?;
        let second = client.wait_for_start(2)?;
        if prompt_of(&second) != Some(reply::prompt(&flower)) {
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
        if prompt_of(&third) != Some(reply::prompt(&Character::standard())) {
            return Err(format!("the third claude should be 小克's again, it was started with {:?}", prompt_of(&third)));
        }

        // The longest persona a profile may carry, on a command line with the schema and the rules.
        let longest = "字".repeat(2000);
        client.character("巨大", &longest, None)?;
        let fourth = client.wait_for_start(4)?;
        if prompt_of(&fourth) != Some(reply::prompt(&Character::new("巨大", &longest, None)?)) {
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
        if starts.len() != 1 || prompt_of(&starts[0]) != Some(reply::prompt(&Character::standard())) {
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
        if starts.len() != 2 || prompt_of(&starts[1]) != Some(reply::prompt(&Character::new("阿花", "愛說話。", None)?)) {
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
