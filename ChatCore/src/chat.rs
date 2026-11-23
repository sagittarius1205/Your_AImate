// The OpenAI-compatible chat engine: `POST {base}/chat/completions`, the whole answer in one
// response. One wire for every service that speaks it (Ollama, LM Studio, OpenAI, OpenRouter,
// Gemini's compatibility layer), told apart only by the address, the model and the key's variable.
//
// The character gets no tools by construction: the request carries no `tools` and no `tool_choice`,
// so the model has nothing to call. A response that calls one anyway, or refuses, ends the turn.
// What the model says goes to `reply::parse` like every other engine's answer, so nothing in this
// file decides what an answer means.
//
// Unlike `claude`, nothing on the other side remembers the conversation. `History` is that memory,
// held by the engine thread, bounded, and emptied when she becomes someone else.

use crate::claude::Failure;
use crate::reply;
use crate::voice::{self, Endpoint, Key, KeyFault};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// Which engine the core runs: `claude` or `openai`. `--engine` on the command line wins over it.
pub const ENGINE_ENV: &str = "VRCHATCORE_ENGINE";
/// The service's base address, as for speech: `https://` anywhere, plain `http://` on loopback.
pub const URL_ENV: &str = "VRCHATCORE_CHAT_URL";
/// The `model` the service is sent. `VRCHATCORE_MODEL` stays `claude`'s.
pub const MODEL_ENV: &str = "VRCHATCORE_CHAT_MODEL";
/// The NAME of the variable that holds the key, never the key.
pub const KEY_ENV_ENV: &str = "VRCHATCORE_CHAT_KEY_ENV";
/// Ollama on this machine, which is what an address left out most likely means.
pub const DEFAULT_BASE: &str = "http://127.0.0.1:11434/v1";

/// Turns of history sent with each new sentence. Every turn is sent again each time, so the cost
/// and the wait grow with it; eight is a guess at "remembers the last few things said", not a
/// measured limit.
pub const HISTORY_TURNS: usize = 8;

const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const REMOTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Listing the models is a page of names, not an answer, so it is given little time.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a waiting turn looks up to see whether the core is shutting down.
const POLL: Duration = Duration::from_millis(50);

/// What the four variables say, as they were in the environment.
pub struct ChatEnv {
    pub url: Option<OsString>,
    pub model: Option<OsString>,
    pub key_env: Option<OsString>,
}

impl ChatEnv {
    pub fn from_process() -> ChatEnv {
        ChatEnv { url: std::env::var_os(URL_ENV), model: std::env::var_os(MODEL_ENV), key_env: std::env::var_os(KEY_ENV_ENV) }
    }

    /// The variable that holds the key, which `claude` is started without.
    pub fn key_env(&self) -> Option<&OsStr> {
        self.key_env.as_deref().filter(|v| !v.is_empty())
    }
}

/// An address and the key to show it, checked.
#[derive(Debug)]
pub struct Server {
    /// Without a trailing slash; each request adds its own path.
    base: Endpoint,
    key: Option<Key>,
}

impl Server {
    pub fn read(env: &ChatEnv, lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Server, String> {
        let value = env.url.as_ref().filter(|v| !v.is_empty()).map_or_else(|| DEFAULT_BASE.to_string(), |v| v.to_string_lossy().into_owned());
        let base = voice::endpoint(&value, "").ok_or_else(|| {
            format!(
                "環境變數 {URL_ENV} 是 {:?}，這裡只接受 https:// 開頭的位址，或 http://127.0.0.1、http://[::1] 這種本機位址。用 http 連別台機器，你說的每一句話都會在網路上被看光。",
                voice::shown(&value)
            )
        })?;
        if base.tls && !cfg!(feature = "tls") {
            return Err(format!("{URL_ENV} 指到 {}，但這個核心編譯時沒有帶 TLS（cargo build --features tls），連不了 https。", base.url));
        }
        let key = match env.key_env() {
            None => None,
            Some(name) => Some(voice::key_from(name, lookup).map_err(|fault| match fault {
                KeyFault::Name => format!("環境變數 {KEY_ENV_ENV} 要填放金鑰的環境變數的名稱，不是金鑰本身：只能有大寫英文字母、數字和底線，不超過 40 個字元。"),
                KeyFault::Missing(name) => format!("金鑰放在環境變數 {name}（{KEY_ENV_ENV} 指到它），但它沒有設值。把金鑰放進 {name} 再重開桌寵，或不要設 {KEY_ENV_ENV}。"),
                KeyFault::Unusable(name) => format!("環境變數 {name} 裡的金鑰不能放進請求標頭：它只能有可見的英文字母、數字和符號，不能有空白或換行。"),
            })?),
        };
        Ok(Server { base, key })
    }

    fn agent(&self, limit: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_connect(Some(if self.base.local { LOCAL_CONNECT_TIMEOUT } else { REMOTE_CONNECT_TIMEOUT }))
            .timeout_global(Some(limit))
            .http_status_as_error(false)
            // A redirect would carry the key to a host the user did not name.
            .max_redirects(0)
            // As for speech (voice.rs): loopback never goes through a proxy, and neither does the rest.
            .proxy(None)
            .build()
            .new_agent()
    }

    /// What the bubble says when nothing answered at `url`.
    fn no_answer(&self, url: &str) -> String {
        if self.base.local {
            format!("本機聊天服務沒有開（{url} 連不上）。用 Ollama 的話，先打開 Ollama，等它跑起來再試一次。")
        } else {
            format!("聊天服務 {url} 連不上（網路、網址或憑證有問題）。")
        }
    }

    fn fault(&self, url: &str, error: ureq::Error, limit: Duration) -> String {
        match error {
            ureq::Error::Timeout(ureq::Timeout::Connect) | ureq::Error::ConnectionFailed => self.no_answer(url),
            ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => self.no_answer(url),
            ureq::Error::Timeout(_) => format!("聊天服務 {} 秒內沒有回答，所以這句沒有回話。", limit.as_secs()),
            other => format!("跟聊天服務的連線出了問題（{other}）。"),
        }
    }

    /// What a status other than 200 says, with the key taken out of it.
    fn refused(&self, code: u16, body: &[u8]) -> String {
        let said = String::from_utf8_lossy(body).into_owned();
        let said = self.key.as_ref().map_or(said.clone(), |key| key.redact(&said));
        let said: String = said.chars().take(160).collect();
        if matches!(code, 401 | 403) {
            format!("聊天服務回了 HTTP {code}（{said}），它不接受這把金鑰。檢查 {KEY_ENV_ENV} 指到的那個環境變數。")
        } else {
            format!("聊天服務回了 HTTP {code}（{said}）。")
        }
    }
}

/// The server and the model, which is what a turn needs.
#[derive(Debug)]
pub struct Service {
    server: Server,
    model: String,
}

impl Service {
    pub fn read(env: &ChatEnv, lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Service, String> {
        let server = Server::read(env, lookup)?;
        let model = env.model.as_ref().filter(|v| !v.is_empty()).ok_or_else(|| format!("沒有指定聊天模型：{MODEL_ENV} 是空的。在設定的模型分頁選一個模型。"))?;
        let model = model
            .to_str()
            .filter(|m| voice::is_name(m))
            .ok_or_else(|| format!("環境變數 {MODEL_ENV} 不是模型名稱：它是 1 到 64 個字元，只能有英文字母、數字和 . _ - : /。"))?;
        Ok(Service { server, model: model.to_string() })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn url(&self) -> String {
        format!("{}/chat/completions", self.server.base.url)
    }

    /// The request body for `text`, said after `history`, to the character `system` describes.
    ///
    /// `reasoning_effort: "none"` goes only to this machine. Measured on Ollama 0.40.11 with
    /// gemma4 (2026-11-09): without it the model thinks first, 2546 tokens and 137 s for one short
    /// answer; with it, 76 tokens and 3.1 s. Ollama's own `think: false` did not stop it through this
    /// endpoint. A service elsewhere may refuse the field for a model that has no such setting, and
    /// whether it does is not measured, so it is not sent there.
    pub fn body(&self, system: &str, history: &History, text: &str) -> Value {
        let mut messages = vec![json!({ "role": "system", "content": system })];
        for (said, answered) in &history.turns {
            messages.push(json!({ "role": "user", "content": said }));
            messages.push(json!({ "role": "assistant", "content": answered }));
        }
        messages.push(json!({ "role": "user", "content": text }));
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "reply", "schema": reply::schema(reply::Relaying::Off), "strict": true },
            },
        });
        if self.server.base.local {
            body["reasoning_effort"] = json!("none");
        }
        body
    }
}

/// What she and the user said, oldest first: each turn is the sentence and the answer exactly as the
/// model wrote it, which is the JSON `parse` accepted. A turn that failed is not kept, so the model
/// never reads an answer the core refused as one it gave.
#[derive(Default)]
pub struct History {
    turns: VecDeque<(String, String)>,
}

impl History {
    pub fn push(&mut self, said: &str, answered: &str) {
        if self.turns.len() == HISTORY_TURNS {
            self.turns.pop_front();
        }
        self.turns.push_back((said.to_string(), answered.to_string()));
    }

    pub fn clear(&mut self) {
        self.turns.clear();
    }

    pub fn len(&self) -> usize {
        self.turns.len()
    }
}

/// The text of the model's answer, out of the response's envelope: `choices[0].message.content`.
/// A tool call or a refusal is not an answer, whatever else the message holds.
pub fn answer(response: &Value) -> Result<String, String> {
    let message = &response["choices"][0]["message"];
    if !message.is_object() {
        return Err(format!("聊天服務的回應裡沒有 choices[0].message：{}", reply::excerpt(&response.to_string())));
    }
    if message.get("tool_calls").is_some_and(|calls| !calls.is_null() && calls.as_array().is_none_or(|a| !a.is_empty())) {
        return Err("模型要求呼叫工具，但她沒有任何工具，所以這一輪作廢。".to_string());
    }
    if let Some(refusal) = message.get("refusal").filter(|r| !r.is_null()) {
        let said = refusal.as_str().map_or_else(|| refusal.to_string(), str::to_string);
        return Err(format!("模型拒絕回答：{}", reply::excerpt(&said)));
    }
    match message.get("content") {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(other) => Err(format!("模型的回答是 {}，不是文字", reply::kind(other))),
        None => Err("聊天服務的回應裡沒有 content".to_string()),
    }
}

/// The client for one chat service, made once when the core starts.
pub struct Client {
    service: Result<Service, String>,
    limit: Duration,
}

impl Client {
    pub fn new(env: &ChatEnv, lookup: &dyn Fn(&str) -> Option<OsString>, limit: Duration) -> Client {
        Client { service: Service::read(env, lookup), limit }
    }

    pub fn service(&self) -> Result<&Service, &String> {
        self.service.as_ref()
    }

    /// One sentence and the history before it, answered with the model's text. The request runs on
    /// a thread of its own so a stop is seen within `POLL`; a request given up on is left to end at
    /// its own timeout, which is this same `limit`.
    pub fn ask(&self, system: &str, history: &History, text: &str, stop: &AtomicBool) -> Result<String, Failure> {
        let service = self.service.as_ref().map_err(|why| Failure::Broken(why.clone()))?;
        let server = &service.server;
        let url = service.url();
        let body = service.body(system, history, text).to_string();
        let agent = server.agent(self.limit);
        let authorization = server.key.as_ref().map(Key::bearer);
        let (tx, rx) = mpsc::channel();
        let target = url.clone();
        thread::spawn(move || {
            let mut post = agent.post(&target).header("Content-Type", "application/json");
            if let Some(authorization) = &authorization {
                post = post.header("Authorization", authorization);
            }
            let got = post.send(body).and_then(|mut response| {
                let code = response.status().as_u16();
                response.body_mut().read_to_vec().map(|bytes| (code, bytes))
            });
            let _ = tx.send(got);
        });
        let deadline = Instant::now() + self.limit;
        let got = loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Failure::Stopped);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::Broken(format!("聊天服務 {} 秒內沒有回答，所以這句沒有回話。", self.limit.as_secs())));
            }
            match rx.recv_timeout(left.min(POLL)) {
                Ok(got) => break got,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(Failure::Broken("聊天服務的請求在半路消失了".to_string())),
            }
        };
        let (code, bytes) = got.map_err(|e| Failure::Broken(server.fault(&url, e, self.limit)))?;
        if code != 200 {
            return Err(Failure::Broken(server.refused(code, &bytes)));
        }
        let response: Value = serde_json::from_slice(&bytes)
            .map_err(|e| Failure::Broken(format!("聊天服務回的不是 JSON（{e}）：{}", reply::excerpt(&String::from_utf8_lossy(&bytes)))))?;
        answer(&response).map_err(Failure::Broken)
    }
}

/// The models the service at `env`'s address has, by `GET {base}/models`, sorted. `vrchatcore
/// models` prints this for the settings window, so the page never reaches the service itself.
pub fn models(env: &ChatEnv, lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Vec<String>, String> {
    let server = Server::read(env, lookup)?;
    let url = format!("{}/models", server.base.url);
    let mut get = server.agent(LIST_TIMEOUT).get(&url);
    if let Some(key) = &server.key {
        get = get.header("Authorization", key.bearer());
    }
    let mut response = get.call().map_err(|e| server.fault(&url, e, LIST_TIMEOUT))?;
    let code = response.status().as_u16();
    let bytes = response.body_mut().read_to_vec().map_err(|e| server.fault(&url, e, LIST_TIMEOUT))?;
    if code != 200 {
        return Err(server.refused(code, &bytes));
    }
    let listing: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{url} 回的不是 JSON（{e}）"))?;
    let Some(data) = listing["data"].as_array() else {
        return Err(format!("{url} 回的清單裡沒有 data 陣列，它可能不是 OpenAI 相容的服務"));
    };
    let mut names: Vec<String> = data.iter().filter_map(|m| m["id"].as_str()).filter(|id| voice::is_name(id)).map(str::to_string).collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// `vrchatcore models`: one JSON line, `{"models":[...]}` or `{"error":"..."}`, from the same four
/// variables `serve` reads. The exit code says which.
pub fn print_models() -> std::process::ExitCode {
    let (line, code) = match models(&ChatEnv::from_process(), &|name| std::env::var_os(name)) {
        Ok(names) => (json!({ "models": names }), std::process::ExitCode::SUCCESS),
        Err(why) => (json!({ "error": why }), std::process::ExitCode::FAILURE),
    };
    println!("{line}");
    code
}
