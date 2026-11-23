//! Passing a message to another Claude session (Docs/DESIGN_Pet.md 6.2). The user asks her, or presses
//! 轉話 on a row of the session list, and a sheet shows the exact target and the exact text. Only the
//! sheet's 送出, a click, sends, and one click sends one message. What she says about it afterwards
//! comes from the core's `relayed`, in fixed words, never from a model.
//!
//! The core delivers (ChatCore, `serve --relay`). The pet holds the draft, its revision, its timers and
//! the outcome in one `Machine`, and the page draws the `View` it is sent and hands back clicks. The
//! wire is restated here the way `core.rs` restates the rest of it, and `pet_relay` pins it to the
//! literal lines of the design:
//!
//! - `{"op":"relay","nonce","title","body","summary"}`, written once per approval (`core::relay_line`).
//! - `{"ev":"relay_asked","to","text"}`: she was asked to pass something on. `turn_end` follows and no
//!   utterance, so nothing she wrote that turn is said.
//! - `{"ev":"relay_step","nonce","step":"listing"}`, then `{"ev":"relay_step","nonce","step":"handing","to"}`.
//!   `handing` comes before the courier's model is given the message, so from then on it may arrive.
//! - `{"ev":"relayed","nonce","outcome":{"kind",...},"gated","elapsed_ms","cost_usd"}`, once per op.
//!
//! The first half is pure, like `core.rs` and `hit.rs`: no tauri type appears in it, so `pet_relay`
//! drives it with values. The second half is the process side, kept thin.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

/// The words above every relayed message. They are in the bytes the user approves, so the confirm
/// covers what leaves, and the receiving session reads where the message came from.
pub const HEADER: &str = "這是你的使用者透過桌面寵物 AImate 轉來的話。送出前他看過這段文字並按了送出。";
/// The `summary` SendMessage carries, the same for every relay.
pub const SUMMARY: &str = "使用者透過 AImate 轉話";
/// Characters of the user's text. With the header and its blank line the body stays inside the
/// core's 1200.
pub const TEXT_MAX: usize = 1000;
/// The core refuses a title longer than this or with a control character in it, so such a row is
/// listed and cannot be picked.
pub const TITLE_MAX: usize = 200;
/// What she heard for the target, shown on the sheet when nothing was chosen.
pub const HINT_MAX: usize = 64;
/// A draft nobody sends is put away. Counted from when it opened or was last replaced.
pub const DRAFT_LIFE: Duration = Duration::from_secs(180);
pub const DELIVERED_SHOWN: Duration = Duration::from_secs(20);
/// Longer than the core's two steps of 45 s, so it fires only when the core went quiet.
pub const WATCHDOG: Duration = Duration::from_secs(150);
/// How long after the sheet changed under the pointer an approval is refused. A click aimed at the
/// sheet as it was, landing a moment after Rust replaced or redrew it, would otherwise approve what the
/// user never read: 送出 sits on the same pixel in both. Longer than a click's travel and a redraw, far
/// shorter than reading a sheet.
pub const SETTLE: Duration = Duration::from_millis(800);
/// The page's event, carrying the `View`.
pub const STATE_EVENT: &str = "relay-state";
const SPOKEN_TITLE_MAX: usize = 24;

// ---------------------------------------------------------------------------------------------
// The pure half

/// Which draft a click was made on. Every change the page could act on takes a new one, so a click
/// made on a sheet that has since changed is refused instead of approving something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Revision(u64);

/// The id of one approval on the wire. Unique across restarts of the core, because the core refuses
/// a nonce it has seen and the pet's counter starts again with each pet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Nonce(String);

impl Nonce {
    /// `r-<pid>-<ms>-<n>`, which is `[a-z0-9-]` and at most 40 characters for any pid, any time
    /// before the year 2286 and a million approvals.
    pub fn mint(pid: u32, ms: u64, n: u64) -> Nonce {
        Nonce(format!("r-{pid}-{ms}-{n}"))
    }

    /// One the core sent back. It is only ever compared with the one in flight.
    pub fn from_wire(s: &str) -> Nonce {
        Nonce(s.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Origin {
    /// She was asked, and the core sent `relay_asked`.
    Asked,
    /// The user pressed 轉話 on a row.
    Row,
}

/// A session a relay can be sent to: named, with a name no other row has.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub session_id: String,
    pub title: String,
    pub cwd: String,
    pub status: String,
    pub started_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Unpickable {
    /// ListAgents addresses a session by its name, and what it prints for one with none is not known.
    Unnamed,
    /// Two rows with one name cannot be told apart in the listing the core addresses from.
    SameTitle,
    /// A name with a line break or a control character in it, or longer than `TITLE_MAX`, which the
    /// core refuses.
    Unsendable,
}

/// One row of the session list, as the sheet lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub session_id: String,
    pub title: Option<String>,
    pub cwd: String,
    pub status: String,
    pub started_at: i64,
    pub unpickable: Option<Unpickable>,
    /// What she heard names this row.
    pub matched: bool,
}

impl Choice {
    fn target(&self) -> Option<Target> {
        if self.unpickable.is_some() {
            return None;
        }
        Some(Target {
            session_id: self.session_id.clone(),
            title: self.title.clone()?,
            cwd: self.cwd.clone(),
            status: self.status.clone(),
            started_at: self.started_at,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Pick {
    /// Every row, the ones the hint names first. `chosen` is set when exactly one pickable row is
    /// named, or the row whose 轉話 was pressed.
    Rows { chosen: Option<String>, choices: Vec<Choice> },
    /// The session list could not be read.
    Failed { why: String },
}

pub type Fleet = Result<Vec<crate::Session>, String>;

/// Words that name a session without saying which: 「AImate 那個 session」 is AImate.
const FILLER: &[&str] = &["session", "那一個", "那個", "這一個", "這個", "視窗", "對話", "的"];

/// Case, full-width ASCII and every mark folded away, each mark a space.
fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let c = match u32::from(c) {
            wide @ 0xFF01..=0xFF5E => char::from_u32(wide - 0xFEE0).unwrap_or(c),
            _ => c,
        };
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else {
            out.push(' ');
        }
    }
    out
}

fn squashed(text: &str) -> String {
    fold(text).split_whitespace().collect()
}

fn words(hint: &str) -> Vec<String> {
    let mut folded = fold(hint);
    for filler in FILLER {
        folded = folded.replace(filler, " ");
    }
    folded.split_whitespace().map(str::to_string).collect()
}

fn leaf(cwd: &str) -> &str {
    cwd.rsplit(['\\', '/']).find(|part| !part.is_empty()).unwrap_or("")
}

fn sendable(title: &str) -> bool {
    title.chars().count() <= TITLE_MAX && !title.chars().any(char::is_control)
}

fn choices(rows: &[crate::Session]) -> Vec<Choice> {
    let title = |s: &crate::Session| s.name.clone().filter(|n| !n.trim().is_empty());
    rows.iter()
        .map(|s| {
            let named = title(s);
            let unpickable = match &named {
                None => Some(Unpickable::Unnamed),
                Some(t) if !sendable(t) => Some(Unpickable::Unsendable),
                Some(t) if rows.iter().filter(|o| title(o).as_deref() == Some(t)).count() > 1 => Some(Unpickable::SameTitle),
                Some(_) => None,
            };
            Choice {
                session_id: s.session_id.clone(),
                title: named,
                cwd: s.cwd.clone(),
                status: s.status.clone(),
                started_at: s.started_at,
                unpickable,
                matched: false,
            }
        })
        .collect()
}

/// The rows `hint` names. A title equal to it wins, then titles holding every word of it, then folder
/// names holding every word of it. One pickable row named is chosen. Every row is listed, the named
/// ones first. No dependency, so simplified and traditional Chinese are not folded into each other,
/// and a hint that differs that way names nothing and the user chooses.
pub fn pick(hint: &str, rows: &[crate::Session]) -> Pick {
    let mut all = choices(rows);
    let whole = squashed(hint);
    let words = words(hint);
    let holds = |hay: &str| !words.is_empty() && words.iter().all(|w| squashed(hay).contains(w.as_str()));
    let tiers: [&dyn Fn(&Choice) -> bool; 3] = [
        &|c| !whole.is_empty() && c.title.as_deref().is_some_and(|t| squashed(t) == whole),
        &|c| c.title.as_deref().is_some_and(|t| holds(t)),
        &|c| holds(leaf(&c.cwd)),
    ];
    if let Some(tier) = tiers.iter().find(|tier| all.iter().any(|c| tier(c))) {
        for c in &mut all {
            c.matched = tier(c);
        }
    }
    let named: Vec<&Choice> = all.iter().filter(|c| c.matched && c.unpickable.is_none()).collect();
    let chosen = (named.len() == 1).then(|| named[0].session_id.clone());
    let (mut first, rest): (Vec<Choice>, Vec<Choice>) = all.into_iter().partition(|c| c.matched);
    first.extend(rest);
    Pick::Rows { chosen, choices: first }
}

/// The rows with `session_id` chosen when it can be, for a row's 轉話.
fn opened(session_id: &str, rows: &[crate::Session]) -> Pick {
    let all = choices(rows);
    let chosen = all.iter().find(|c| c.session_id == session_id && c.unpickable.is_none()).map(|c| c.session_id.clone());
    let (mut first, rest): (Vec<Choice>, Vec<Choice>) = all.into_iter().partition(|c| c.session_id == session_id);
    first.extend(rest);
    Pick::Rows { chosen, choices: first }
}

/// `HEADER`, a blank line, and the text, trimmed. Holding one is the proof the limits held.
#[derive(Clone, Debug, PartialEq)]
pub struct Body(String);

impl Body {
    pub fn new(text: &str) -> Result<Body, String> {
        let text = text.replace("\r\n", "\n");
        let text = text.trim();
        let chars = text.chars().count();
        if chars == 0 {
            return Err("要轉的話是空的".to_string());
        }
        if chars > TEXT_MAX {
            return Err(format!("要轉的話有 {chars} 個字，最多 {TEXT_MAX} 個"));
        }
        if text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return Err("要轉的話裡有看不見的控制字元".to_string());
        }
        Ok(Body(format!("{HEADER}\n\n{text}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The user's text, without the header.
    pub fn text(&self) -> &str {
        &self.0[HEADER.len() + 2..]
    }
}

/// One approved delivery, as `core::relay_line` writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct Order {
    pub nonce: Nonce,
    pub title: String,
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq)]
struct Approved {
    revision: Revision,
    nonce: Nonce,
    origin: Origin,
    target: Target,
    body: Body,
}

impl Approved {
    fn order(&self) -> Order {
        Order { nonce: self.nonce.clone(), title: self.target.title.clone(), body: self.body.clone() }
    }
}

/// Why nothing was sent, in the core's words (`why` on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    Off,
    Busy,
    Repeated,
    NoClaude,
    GateAbsent,
    NotFound,
    Ambiguous,
    ModelDidNotCall,
    Refused,
    Failed,
    Stopped,
}

impl Why {
    pub const ALL: [Why; 11] = [
        Why::Off,
        Why::Busy,
        Why::Repeated,
        Why::NoClaude,
        Why::GateAbsent,
        Why::NotFound,
        Why::Ambiguous,
        Why::ModelDidNotCall,
        Why::Refused,
        Why::Failed,
        Why::Stopped,
    ];

    pub fn wire(self) -> &'static str {
        match self {
            Why::Off => "off",
            Why::Busy => "busy",
            Why::Repeated => "repeated",
            Why::NoClaude => "no_claude",
            Why::GateAbsent => "gate_absent",
            Why::NotFound => "not_found",
            Why::Ambiguous => "ambiguous",
            Why::ModelDidNotCall => "model_did_not_call",
            Why::Refused => "refused",
            Why::Failed => "failed",
            Why::Stopped => "stopped",
        }
    }

    pub fn parse(s: &str) -> Option<Why> {
        Why::ALL.into_iter().find(|w| w.wire() == s)
    }

    /// What the sheet says it means.
    fn reason(self) -> &'static str {
        match self {
            Why::Off => "對話核心沒有開轉話。",
            Why::Busy => "上一則還在送。",
            Why::Repeated => "核心已經收過這一則，這次沒有再送。",
            Why::NoClaude => "轉話要用 claude，這台電腦找不到它。",
            Why::GateAbsent => "核准的關卡沒有接上，所以什麼都沒送。",
            Why::NotFound => "找不到這個 session，它可能已經關掉或改了名字。",
            Why::Ambiguous => "有兩個以上同名的 session，分不出是哪一個。",
            Why::ModelDidNotCall => "負責送信的模型沒有照核准的內容送，所以什麼都沒送。",
            Why::Refused => "對方的收件匣沒有收。",
            Why::Failed => "送的過程出了錯。",
            Why::Stopped => "對話核心停下來了。",
        }
    }
}

/// The core's outcome, restated from the wire, under the wire's own names.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// In the other session's inbox, unread.
    Delivered { to: String, msg_id: String },
    /// Nothing left the machine.
    NotSent { why: Why, detail: String },
    /// It may have arrived.
    Unknown { detail: String },
    /// Something other than what was approved was sent. A fault, said with what was sent.
    SentOther { to: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "camelCase")]
pub enum Step {
    /// The op is written and the core has not said it started.
    Starting,
    Listing,
    /// The address the courier is handing it to. From here on it may arrive.
    Handing { to: String },
}

impl Step {
    fn rank(&self) -> u8 {
        match self {
            Step::Starting => 0,
            Step::Listing => 1,
            Step::Handing { .. } => 2,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Step::Starting => "剛送出去、核心還沒回",
            Step::Listing => "正在找那個 session",
            Step::Handing { .. } => "已經交給送信的模型",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Draft {
    revision: Revision,
    /// The revision it opened with. A redraw keeps it, so a timer started when it opened still
    /// matches, and a replaced draft gets a new one, so the old timer does not.
    opened: Revision,
    origin: Origin,
    hint: Option<String>,
    pick: Pick,
    text: String,
    /// When an approval is taken again: `SETTLE` after it opened, was replaced or was redrawn.
    armed_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Idle,
    Drafting(Draft),
    Sending { approved: Approved, step: Step, since_ms: u64 },
    Done { approved: Approved, outcome: Outcome, gated: Option<bool>, armed_ms: u64 },
}

#[derive(Clone, Debug)]
pub enum Input {
    /// From the core's `relay_asked`, with the session list read off the core's reader thread.
    Asked { to: String, text: String, fleet: Fleet },
    /// A row's 轉話.
    Opened { session_id: String, fleet: Fleet },
    /// 送出 in `Drafting`, or 再送一次 in `Done`. The only input that reaches `Sending`, and only the
    /// `relay_approve` command makes one, which only a click on the page calls.
    Approve { revision: u64, session_id: String, text: String, fleet: Fleet, nonce: Nonce },
    /// 取消 in `Drafting`, 關閉 in `Done`.
    Dismiss { revision: u64 },
    Expire { revision: Revision },
    Close { revision: Revision },
    Step { nonce: Nonce, step: Step },
    Finished { nonce: Nonce, outcome: Outcome, gated: Option<bool>, elapsed_ms: Option<u64>, cost_usd: Option<f64> },
    /// The core stopped or is being restarted.
    CoreGone { why: String },
    Watchdog { nonce: Nonce },
}

#[derive(Clone, Debug)]
pub enum Effect {
    /// Through `Core::greet`, which the core voices without asking the model.
    Say(String),
    /// One `relay` line. A write that fails comes back as `Finished` with `not_sent`.
    Deliver(Order),
    Audit(Record),
    /// Applied after `delay`. Each carries its revision or nonce, so one that outlived its draft does
    /// nothing.
    After { delay: Duration, input: Input },
}

/// One line of `<home>/relay.jsonl`. No body: what the user wrote to another session is not kept
/// (DESIGN_ChatCore.md 「不記內容」), only how long it was.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Record {
    pub at: u64,
    pub nonce: Nonce,
    pub origin: Origin,
    pub session_id: String,
    pub title: String,
    pub to: Option<String>,
    pub body_chars: usize,
    pub outcome: Kept,
    pub msg_id: Option<String>,
    pub gated: Option<bool>,
    pub elapsed_ms: u64,
    pub cost_usd: Option<f64>,
}

/// The outcome as the audit keeps it: the kind and, for nothing sent, the core's reason. Never the
/// detail or a message, which can carry text from another session or the body itself.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kept {
    Delivered,
    NotSent { why: Why },
    Unknown,
    SentOther,
}

/// The page's wire. What the sheet draws and the revision to send back, nothing it could act on
/// wrongly.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum View {
    Idle,
    Drafting {
        revision: Revision,
        draft: Revision,
        origin: Origin,
        hint: Option<String>,
        pick: Pick,
        header: &'static str,
        summary: &'static str,
        text: String,
        text_max: usize,
        armed_at_ms: u64,
    },
    Sending { revision: Revision, target: Target, text: String, step: Step, since_ms: u64 },
    Done {
        revision: Revision,
        target: Target,
        text: String,
        outcome: Outcome,
        gated: Option<bool>,
        headline: &'static str,
        reason: String,
        retry: bool,
        armed_at_ms: u64,
    },
}

const NOT_RELAYING: &str = "現在沒有在轉話";
const STALE: &str = "這則轉話已經換過了，看一下新的再按";
const SENDING: &str = "正在送，不能改也不能取消";
const SETTLING: &str = "這則剛換過，看清楚再按";

/// The relay, one at a time. `apply` is every transition and is pure: the time is passed in.
#[derive(Debug)]
pub struct Machine {
    phase: Phase,
    next: u64,
}

impl Default for Machine {
    fn default() -> Machine {
        Machine { phase: Phase::Idle, next: 0 }
    }
}

impl Machine {
    fn bump(&mut self) -> Revision {
        self.next += 1;
        Revision(self.next)
    }

    /// `Err` is a refusal for the command that asked. It leaves the phase as it was, except that an
    /// approval whose row has gone, been renamed or gained a twin redraws the sheet from the fresh
    /// list under a new revision, so the user sees what changed before anything is sent.
    pub fn apply(&mut self, input: Input, now_ms: u64) -> Result<Vec<Effect>, String> {
        let phase = std::mem::replace(&mut self.phase, Phase::Idle);
        let (next, result) = self.step(phase, input, now_ms);
        self.phase = next;
        result
    }

    fn step(&mut self, phase: Phase, input: Input, now: u64) -> (Phase, Result<Vec<Effect>, String>) {
        match (phase, input) {
            (sending @ Phase::Sending { .. }, Input::Asked { .. }) => (sending, Ok(vec![Effect::Say(line(Said::Busy))])),
            (Phase::Drafting(_), Input::Asked { to, text, fleet }) => self.open(Origin::Asked, &to, text, fleet, None, true, now),
            (Phase::Idle | Phase::Done { .. }, Input::Asked { to, text, fleet }) => {
                self.open(Origin::Asked, &to, text, fleet, None, false, now)
            }
            (sending @ Phase::Sending { .. }, Input::Opened { .. }) => (sending, Err(SENDING.to_string())),
            (_, Input::Opened { session_id, fleet }) => self.open(Origin::Row, "", String::new(), fleet, Some(&session_id), false, now),

            (Phase::Drafting(d), Input::Approve { revision, session_id, text, fleet, nonce }) => {
                self.approve_draft(d, revision, &session_id, &text, fleet, nonce, now)
            }
            (Phase::Done { approved, outcome, gated, armed_ms }, Input::Approve { revision, session_id, fleet, nonce, .. }) => {
                self.approve_again((approved, outcome, gated, armed_ms), revision, &session_id, fleet, nonce, now)
            }
            (sending @ Phase::Sending { .. }, Input::Approve { .. } | Input::Dismiss { .. }) => (sending, Err(SENDING.to_string())),
            (Phase::Idle, Input::Approve { .. } | Input::Dismiss { .. }) => (Phase::Idle, Err(NOT_RELAYING.to_string())),

            (Phase::Drafting(d), Input::Dismiss { revision }) if d.revision.0 == revision => {
                (Phase::Idle, Ok(vec![Effect::Say(line(Said::Cancelled))]))
            }
            (Phase::Done { approved, .. }, Input::Dismiss { revision }) if approved.revision.0 == revision => (Phase::Idle, Ok(vec![])),
            (phase @ (Phase::Drafting(_) | Phase::Done { .. }), Input::Dismiss { .. }) => (phase, Err(STALE.to_string())),

            (Phase::Drafting(d), Input::Expire { revision }) if d.opened == revision => {
                (Phase::Idle, Ok(vec![Effect::Say(line(Said::Expired))]))
            }
            (Phase::Done { approved, .. }, Input::Close { revision }) if approved.revision == revision => (Phase::Idle, Ok(vec![])),

            (Phase::Sending { approved, step, since_ms }, Input::Step { nonce, step: next }) if nonce == approved.nonce => {
                let step = if next.rank() > step.rank() { next } else { step };
                (Phase::Sending { approved, step, since_ms }, Ok(vec![]))
            }
            (Phase::Sending { approved, step, since_ms }, Input::Finished { nonce, outcome, gated, elapsed_ms, cost_usd })
                if nonce == approved.nonce =>
            {
                let elapsed = elapsed_ms.unwrap_or(now.saturating_sub(since_ms));
                self.finish(approved, &step, outcome, gated, elapsed, cost_usd, now)
            }
            (Phase::Sending { approved, step, since_ms }, Input::CoreGone { why }) => {
                let detail = format!("{why}。那時{}。", step.label());
                self.finish(approved, &step, Outcome::Unknown { detail }, None, now.saturating_sub(since_ms), None, now)
            }
            (Phase::Sending { approved, step, since_ms }, Input::Watchdog { nonce }) if nonce == approved.nonce => {
                let detail = format!("對話核心過了 {} 秒都沒有回這一則。那時{}。", WATCHDOG.as_secs(), step.label());
                self.finish(approved, &step, Outcome::Unknown { detail }, None, now.saturating_sub(since_ms), None, now)
            }

            // A timer or a core line that is not about what is on the sheet now.
            (phase, Input::Expire { .. } | Input::Close { .. } | Input::Step { .. } | Input::Finished { .. } | Input::CoreGone { .. } | Input::Watchdog { .. }) => {
                (phase, Ok(vec![]))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        &mut self,
        origin: Origin,
        hint: &str,
        text: String,
        fleet: Fleet,
        row: Option<&str>,
        replaced: bool,
        now: u64,
    ) -> (Phase, Result<Vec<Effect>, String>) {
        let pick = match (fleet, row) {
            (Err(why), _) => Pick::Failed { why },
            (Ok(rows), Some(id)) => opened(id, &rows),
            (Ok(rows), None) => pick(hint, &rows),
        };
        let hint: String = hint.chars().filter(|c| !c.is_control()).take(HINT_MAX).collect();
        let hint = Some(hint.trim().to_string()).filter(|h| !h.is_empty());
        let revision = self.bump();
        let mut effects = Vec::new();
        if origin == Origin::Asked {
            effects.push(Effect::Say(line(if replaced { Said::Replaced } else { Said::Asked(&pick) })));
        }
        effects.push(Effect::After { delay: DRAFT_LIFE, input: Input::Expire { revision } });
        let armed_ms = now + SETTLE.as_millis() as u64;
        (Phase::Drafting(Draft { revision, opened: revision, origin, hint, pick, text, armed_ms }), Ok(effects))
    }

    #[allow(clippy::too_many_arguments)]
    fn approve_draft(
        &mut self,
        d: Draft,
        revision: u64,
        session_id: &str,
        text: &str,
        fleet: Fleet,
        nonce: Nonce,
        now: u64,
    ) -> (Phase, Result<Vec<Effect>, String>) {
        if d.revision.0 != revision {
            return (Phase::Drafting(d), Err(STALE.to_string()));
        }
        if now < d.armed_ms {
            return (Phase::Drafting(d), Err(SETTLING.to_string()));
        }
        let offered = match &d.pick {
            Pick::Failed { why } => return (Phase::Drafting(d.clone()), Err(format!("看不到 session 清單，沒有送：{why}"))),
            Pick::Rows { choices, .. } => choices.iter().find(|c| c.session_id == session_id).cloned(),
        };
        let Some(chosen) = offered.as_ref().and_then(Choice::target) else {
            return (Phase::Drafting(d), Err("先在上面選一個可以轉的 session".to_string()));
        };
        let body = match Body::new(text) {
            Ok(body) => body,
            Err(why) => return (Phase::Drafting(d), Err(why)),
        };
        let rows = match fleet {
            Ok(rows) => rows,
            Err(why) => return (Phase::Drafting(d), Err(format!("看不到 session 清單，沒有送：{why}"))),
        };
        if let Err(why) = still_there(&chosen, &rows) {
            // The row the user approved stays chosen when it can still take the message, under its
            // new name. Otherwise nothing is: the hint chose for the user once, when the sheet opened,
            // and a redraw must not hand 送出 a row the user turned down or never saw chosen.
            let revision = self.bump();
            let mut pick = pick(d.hint.as_deref().unwrap_or(""), &rows);
            if let Pick::Rows { chosen: kept, choices } = &mut pick {
                let still = choices.iter().any(|c| c.session_id == chosen.session_id && c.unpickable.is_none());
                *kept = still.then(|| chosen.session_id.clone());
            }
            let armed_ms = now + SETTLE.as_millis() as u64;
            let redrawn = Draft { revision, pick, text: text.to_string(), armed_ms, ..d };
            return (Phase::Drafting(redrawn), Err(why));
        }
        let target = fresh(&chosen, &rows);
        self.send(Approved { revision: Revision(0), nonce, origin: d.origin, target, body }, now)
    }

    #[allow(clippy::too_many_arguments)]
    fn approve_again(
        &mut self,
        (approved, outcome, gated, armed_ms): (Approved, Outcome, Option<bool>, u64),
        revision: u64,
        session_id: &str,
        fleet: Fleet,
        nonce: Nonce,
        now: u64,
    ) -> (Phase, Result<Vec<Effect>, String>) {
        let checked = if approved.revision.0 != revision {
            Err(STALE.to_string())
        } else if now < armed_ms {
            Err(SETTLING.to_string())
        } else if !retry(&outcome) {
            Err("這則不能再送：它可能已經送到了".to_string())
        } else if session_id != approved.target.session_id {
            Err("再送一次只能送給同一個 session".to_string())
        } else {
            fleet
                .map_err(|why| format!("看不到 session 清單，沒有送：{why}"))
                .and_then(|rows| still_there(&approved.target, &rows).map(|()| fresh(&approved.target, &rows)))
        };
        match checked {
            Ok(target) => self.send(Approved { nonce, target, ..approved }, now),
            Err(why) => (Phase::Done { approved, outcome, gated, armed_ms }, Err(why)),
        }
    }

    fn send(&mut self, approved: Approved, now: u64) -> (Phase, Result<Vec<Effect>, String>) {
        let approved = Approved { revision: self.bump(), ..approved };
        // The watchdog first, so a write to the core that blocks cannot hold it back.
        let effects = vec![
            Effect::After { delay: WATCHDOG, input: Input::Watchdog { nonce: approved.nonce.clone() } },
            Effect::Say(line(Said::Sending)),
            Effect::Deliver(approved.order()),
        ];
        (Phase::Sending { approved, step: Step::Starting, since_ms: now }, Ok(effects))
    }

    /// Whether the send with `nonce` is still the one in flight. A `Deliver` that runs after the
    /// relay ended, say a restart that put it in `Done` first, is not written.
    pub fn in_flight(&self, nonce: &Nonce) -> bool {
        matches!(&self.phase, Phase::Sending { approved, .. } if &approved.nonce == nonce)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        approved: Approved,
        step: &Step,
        outcome: Outcome,
        gated: Option<bool>,
        elapsed_ms: u64,
        cost_usd: Option<f64>,
        now: u64,
    ) -> (Phase, Result<Vec<Effect>, String>) {
        let (kept, to, msg_id) = match &outcome {
            Outcome::Delivered { to, msg_id } => (Kept::Delivered, Some(to.clone()), Some(msg_id.clone())),
            Outcome::NotSent { why, .. } => (Kept::NotSent { why: *why }, None, None),
            Outcome::Unknown { .. } => (Kept::Unknown, None, None),
            Outcome::SentOther { to, .. } => (Kept::SentOther, Some(to.clone()), None),
        };
        let handed = match step {
            Step::Handing { to } => Some(to.clone()),
            _ => None,
        };
        let record = Record {
            at: now,
            nonce: approved.nonce.clone(),
            origin: approved.origin,
            session_id: approved.target.session_id.clone(),
            title: approved.target.title.clone(),
            to: to.or(handed),
            body_chars: approved.body.as_str().chars().count(),
            outcome: kept,
            msg_id,
            gated,
            elapsed_ms,
            cost_usd,
        };
        let mut effects =
            vec![Effect::Audit(record), Effect::Say(line(Said::Outcome { outcome: &outcome, title: &approved.target.title }))];
        if matches!(outcome, Outcome::Delivered { .. }) {
            effects.push(Effect::After { delay: DELIVERED_SHOWN, input: Input::Close { revision: approved.revision } });
        }
        (Phase::Done { approved, outcome, gated, armed_ms: now + SETTLE.as_millis() as u64 }, Ok(effects))
    }

    pub fn view(&self) -> View {
        match &self.phase {
            Phase::Idle => View::Idle,
            Phase::Drafting(d) => View::Drafting {
                revision: d.revision,
                draft: d.opened,
                origin: d.origin,
                hint: d.hint.clone(),
                pick: d.pick.clone(),
                header: HEADER,
                summary: SUMMARY,
                text: d.text.clone(),
                text_max: TEXT_MAX,
                armed_at_ms: d.armed_ms,
            },
            Phase::Sending { approved, step, since_ms } => View::Sending {
                revision: approved.revision,
                target: approved.target.clone(),
                text: approved.body.text().to_string(),
                step: step.clone(),
                since_ms: *since_ms,
            },
            Phase::Done { approved, outcome, gated, armed_ms } => {
                let (headline, reason) = told(outcome);
                View::Done {
                    revision: approved.revision,
                    target: approved.target.clone(),
                    text: approved.body.text().to_string(),
                    outcome: outcome.clone(),
                    gated: *gated,
                    headline,
                    reason,
                    retry: retry(outcome),
                    armed_at_ms: *armed_ms,
                }
            }
        }
    }
}

/// The approved row in the list as it is now: still there, still called what the user saw, and the
/// only row called that.
fn still_there(target: &Target, rows: &[crate::Session]) -> Result<(), String> {
    let Some(row) = rows.iter().find(|s| s.session_id == target.session_id) else {
        return Err(format!("「{}」這個 session 已經不在了，沒有送", target.title));
    };
    match row.name.as_deref() {
        Some(name) if name == target.title => {}
        Some(name) => return Err(format!("這個 session 改名成「{name}」了，沒有送，看一下再按")),
        None => return Err("這個 session 的名字不見了，沒有送".to_string()),
    }
    if rows.iter().filter(|s| s.name.as_deref() == Some(target.title.as_str())).count() > 1 {
        return Err(format!("現在有兩個以上叫「{}」的 session，分不出是哪一個，沒有送", target.title));
    }
    Ok(())
}

fn fresh(target: &Target, rows: &[crate::Session]) -> Target {
    match rows.iter().find(|s| s.session_id == target.session_id) {
        Some(s) => Target { cwd: s.cwd.clone(), status: s.status.clone(), started_at: s.started_at, ..target.clone() },
        None => target.clone(),
    }
}

/// Approving again is safe only when nothing left the machine and the core did not already take it.
fn retry(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::NotSent { why, .. } if *why != Why::Repeated)
}

/// The sheet's two lines for an outcome: what happened, and what that means.
fn told(outcome: &Outcome) -> (&'static str, String) {
    match outcome {
        Outcome::Delivered { .. } => ("已放進收件匣", "對方還沒讀，也可能不照做。".to_string()),
        Outcome::NotSent { why, .. } => ("沒送出", why.reason().to_string()),
        Outcome::Unknown { .. } => ("不確定對方有沒有收到", "送出那一刻之後就沒有回音了。再送一次，對方可能會收到兩則。".to_string()),
        Outcome::SentOther { .. } => ("送出去的跟你核准的不一樣", "這是錯誤。實際送出去的寫在下面。".to_string()),
    }
}

/// What she says, in fixed words.
pub enum Said<'a> {
    Asked(&'a Pick),
    Replaced,
    Busy,
    Sending,
    Outcome { outcome: &'a Outcome, title: &'a str },
    Cancelled,
    Expired,
}

/// A title as she can say it: no control character, no square bracket, which the core's greeting
/// check refuses, and at most `SPOKEN_TITLE_MAX` characters.
fn spoken(title: &str) -> String {
    let clean: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).filter(|c| !matches!(c, '[' | ']')).collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        return "那個 session".to_string();
    }
    if clean.chars().count() <= SPOKEN_TITLE_MAX {
        return clean;
    }
    let mut cut: String = clean.chars().take(SPOKEN_TITLE_MAX - 1).collect();
    cut.push('…');
    cut
}

pub fn line(said: Said<'_>) -> String {
    match said {
        Said::Asked(Pick::Rows { chosen: Some(id), choices }) => {
            let title = choices.iter().find(|c| &c.session_id == id).and_then(|c| c.title.as_deref()).unwrap_or("");
            format!("要轉給「{}」的話我放在上面了。你看一下，按「送出」我才會送。", spoken(title))
        }
        Said::Asked(Pick::Rows { chosen: None, choices }) if !choices.is_empty() => {
            "要轉給哪一個 session？在上面點一下，再按「送出」。".to_string()
        }
        Said::Asked(_) => "我現在看不到可以轉給誰，原因寫在上面了。".to_string(),
        Said::Replaced => "我改成這一則了，看一下再送。".to_string(),
        Said::Busy => "上一則還在送，等它好了再說一次。".to_string(),
        Said::Sending => "好，我現在送，大概要等十幾秒。".to_string(),
        Said::Outcome { outcome, title } => match outcome {
            Outcome::Delivered { .. } => format!("送到「{}」的收件匣了。它還沒讀，也可能不照做。", spoken(title)),
            Outcome::NotSent { why: Why::NotFound, .. } => {
                format!("沒送出：「{}」這個 session 已經不在了，或改了名字。", spoken(title))
            }
            Outcome::NotSent { why: Why::Ambiguous, .. } => {
                format!("沒送出：有兩個以上叫「{}」的 session，我分不出是哪一個。", spoken(title))
            }
            Outcome::NotSent { why: Why::NoClaude, .. } => "沒送出：轉話要用 claude，這台電腦找不到它。".to_string(),
            Outcome::NotSent { .. } => "沒送出，原因寫在上面了。".to_string(),
            // Not 「有沒有送到」, which says 送到 too: only an arrival may.
            Outcome::Unknown { .. } => "我不確定對方有沒有收到。送出那一刻之後就沒有回音了。再送一次，對方可能會收到兩則。".to_string(),
            Outcome::SentOther { .. } => "出問題了：送出去的跟你核准的不一樣，寫在上面了。".to_string(),
        },
        Said::Cancelled => "好，不送了。".to_string(),
        Said::Expired => "那則轉話放太久，我收起來了，沒有送。".to_string(),
    }
}

/// A relay line of the core's, read whole. Everything else the core prints is `None` here and goes
/// to the page as it always did.
#[derive(Clone, Debug, PartialEq)]
pub enum FromCore {
    Asked { to: String, text: String },
    Step { nonce: Nonce, step: Step },
    Finished { nonce: Nonce, outcome: Outcome, gated: Option<bool>, elapsed_ms: Option<u64>, cost_usd: Option<f64> },
    /// A relay line this cannot use, with why.
    Unreadable(String),
}

impl FromCore {
    /// The machine's input for this line. `fleet` is asked only for an ask, which is the one that
    /// needs the session list.
    pub fn into_input(self, fleet: impl FnOnce() -> Fleet) -> Result<Input, String> {
        match self {
            FromCore::Asked { to, text } => Ok(Input::Asked { to, text, fleet: fleet() }),
            FromCore::Step { nonce, step } => Ok(Input::Step { nonce, step }),
            FromCore::Finished { nonce, outcome, gated, elapsed_ms, cost_usd } => {
                Ok(Input::Finished { nonce, outcome, gated, elapsed_ms, cost_usd })
            }
            FromCore::Unreadable(why) => Err(why),
        }
    }
}

pub fn from_core(event: &Value) -> Option<FromCore> {
    let text = |key: &str| event.get(key).and_then(Value::as_str).map(str::to_string);
    let nonce = text("nonce").filter(|n| !n.is_empty()).map(|n| Nonce::from_wire(&n));
    Some(match event.get("ev").and_then(Value::as_str)? {
        // Read as far as it goes. Whatever she heard, the user sees it on the sheet before anything
        // can be sent, so an ask with a field missing opens an emptier sheet and not silence.
        "relay_asked" => FromCore::Asked { to: text("to").unwrap_or_default(), text: text("text").unwrap_or_default() },
        "relay_step" => {
            let step = match (event.get("step").and_then(Value::as_str), text("to")) {
                (Some("listing"), _) => Some(Step::Listing),
                (Some("handing"), Some(to)) => Some(Step::Handing { to }),
                _ => None,
            };
            match (nonce, step) {
                (Some(nonce), Some(step)) => FromCore::Step { nonce, step },
                _ => FromCore::Unreadable(format!("relay_step 讀不懂：{event}")),
            }
        }
        "relayed" => {
            let Some(nonce) = nonce else {
                return Some(FromCore::Unreadable(format!("relayed 沒有 nonce：{event}")));
            };
            // A result that cannot be read is said as not knowing. Reading it as not sent would offer
            // a second send of something that may have arrived.
            let outcome = outcome_of(event.get("outcome").unwrap_or(&Value::Null))
                .unwrap_or_else(|why| Outcome::Unknown { detail: format!("核心回的轉話結果讀不懂（{why}）") });
            FromCore::Finished {
                nonce,
                outcome,
                gated: event.get("gated").and_then(Value::as_bool),
                elapsed_ms: event.get("elapsed_ms").and_then(Value::as_u64),
                cost_usd: event.get("cost_usd").and_then(Value::as_f64),
            }
        }
        _ => return None,
    })
}

fn outcome_of(v: &Value) -> Result<Outcome, String> {
    let text = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("沒有 {key}"));
    match v.get("kind").and_then(Value::as_str) {
        Some("delivered") => Ok(Outcome::Delivered { to: text("to")?, msg_id: text("msg_id")? }),
        Some("not_sent") => {
            let why = text("why")?;
            let why = Why::parse(&why).ok_or_else(|| format!("why 是 {why:?}"))?;
            Ok(Outcome::NotSent { why, detail: text("detail").unwrap_or_default() })
        }
        Some("unknown") => Ok(Outcome::Unknown { detail: text("detail").unwrap_or_default() }),
        Some("sent_other") => Ok(Outcome::SentOther { to: text("to")?, message: text("message")? }),
        other => Err(format!("kind 是 {other:?}")),
    }
}

// ---------------------------------------------------------------------------------------------
// The process half

/// Tauri state, and the relay's one writer.
#[derive(Default)]
pub struct Relays {
    machine: Mutex<Machine>,
    minted: AtomicU64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Relays {
    pub fn view(&self) -> View {
        self.machine.lock().expect("relay poisoned").view()
    }

    /// Applies, tells the page when the view changed, then runs the effects on a thread of their own,
    /// so a write to the core never holds the lock or the thread that reads the core. The page is told
    /// under the lock: two applies on two threads that told it after unlocking could tell it in the
    /// wrong order, and the sheet would stay on the older view.
    pub fn apply(&self, app: &AppHandle, input: Input) -> Result<(), String> {
        let result = {
            let mut machine = self.machine.lock().expect("relay poisoned");
            let before = machine.view();
            let result = machine.apply(input, now_ms());
            let after = machine.view();
            if after != before
                && let Err(e) = app.emit_to(crate::OVERLAY, STATE_EVENT, &after)
            {
                eprintln!("emit {STATE_EVENT}: {e}");
            }
            result
        };
        let effects = result?;
        if !effects.is_empty() {
            let app = app.clone();
            std::thread::spawn(move || run(&app, effects));
        }
        Ok(())
    }

    /// The one place an `Approve` is made. `relay_approve` is its only caller, and only the page's
    /// 送出 and 再送一次 clicks call that.
    pub fn approve(&self, app: &AppHandle, revision: u64, session_id: String, text: String, fleet: Fleet) -> Result<(), String> {
        let nonce = Nonce::mint(std::process::id(), now_ms(), self.minted.fetch_add(1, Ordering::Relaxed));
        self.apply(app, Input::Approve { revision, session_id, text, fleet, nonce })
    }
}

fn run(app: &AppHandle, effects: Vec<Effect>) {
    for effect in effects {
        match effect {
            Effect::Say(text) => {
                if let Err(why) = app.state::<crate::core::Core>().greet(&text) {
                    eprintln!("relay: not said ({why}): {text}");
                }
            }
            Effect::Deliver(order) => {
                if !app.state::<Relays>().machine.lock().expect("relay poisoned").in_flight(&order.nonce) {
                    eprintln!("relay: {} ended before it was written, so it was not", order.nonce.as_str());
                    continue;
                }
                if let Err(why) = app.state::<crate::core::Core>().relay(&order) {
                    eprintln!("relay: {} did not reach the core: {why}", order.nonce.as_str());
                    let failed = Input::Finished {
                        nonce: order.nonce,
                        outcome: Outcome::NotSent { why: Why::Failed, detail: why },
                        gated: None,
                        elapsed_ms: None,
                        cost_usd: None,
                    };
                    if let Err(why) = app.state::<Relays>().apply(app, failed) {
                        eprintln!("relay: {why}");
                    }
                }
            }
            Effect::Audit(record) => {
                if let Err(why) = audit(&record) {
                    eprintln!("relay: the audit line was not written: {why}");
                }
            }
            Effect::After { delay, input } => {
                let app = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(delay);
                    if let Err(why) = app.state::<Relays>().apply(&app, input) {
                        eprintln!("relay: {why}");
                    }
                });
            }
        }
    }
}

/// A relay line from the core's reader. An ask reads the session list first, which takes a moment,
/// so it goes to a thread of its own. A step or an outcome is applied here, in the order the core
/// printed them, so a core that ends by itself, which this same thread reports as `Down` after its
/// last line, can never overtake one. A restart is reported from the restart's thread before the old
/// core is stopped, so an outcome still in its pipe can arrive after the relay ended as not knowing,
/// and is then ignored: the sheet says less than was known, never more.
pub fn take(app: &AppHandle, from: FromCore) {
    if matches!(from, FromCore::Asked { .. }) {
        let app = app.clone();
        std::thread::spawn(move || {
            let applied = from.into_input(|| crate::fleet(app.state())).and_then(|input| app.state::<Relays>().apply(&app, input));
            if let Err(why) = applied {
                eprintln!("relay: {why}");
            }
        });
        return;
    }
    let applied = from.into_input(|| Err("not asked".to_string())).and_then(|input| app.state::<Relays>().apply(app, input));
    if let Err(why) = applied {
        eprintln!("relay: {why}");
    }
}

/// The core is down or restarting. A relay in flight is not known to have been sent or not.
pub fn core_gone(app: &AppHandle, why: &str) {
    if let Err(why) = app.state::<Relays>().apply(app, Input::CoreGone { why: why.to_string() }) {
        eprintln!("relay: {why}");
    }
}

pub const AUDIT_FILE: &str = "relay.jsonl";
const AUDIT_MAX_BYTES: u64 = 1024 * 1024;

/// Appends one line to `<home>/relay.jsonl`, renaming the file aside past 1 MB the way `vrpet-hook`
/// rotates its log, so a rename that races loses harmlessly.
fn audit(record: &Record) -> Result<(), String> {
    use std::io::Write;
    let dir = crate::home::dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(AUDIT_FILE);
    if std::fs::metadata(&path).map(|m| m.len() > AUDIT_MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("relay.1.jsonl"));
    }
    let mut line = serde_json::to_string(record).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut file =
        std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(line.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
}
