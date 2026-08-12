//! Kill reporting over HTTP.
//!
//! This is a generic kill-reporting webhook: it posts `{token, kills}` to a
//! configured URL and the server resolves everything else. It knows nothing
//! about whatever consumes the reports — no teams, boards, squares, rooms or
//! opponents — and it must stay that way. If this module ever needs that kind
//! of knowledge, that is the signal to split it into its own mod rather than to
//! teach the overlay about one web app.
//!
//! Two properties are load-bearing:
//!
//! * **Read-only.** Flags are only ever read, never written. The same hook that
//!   reads a flag could set one, and that is the line between a tracker and a
//!   cheat tool. Nothing in this module writes to game memory, and no refactor
//!   should change that.
//! * **Full state every send.** Each request carries the complete observed kill
//!   set, not just the newest transition. The server diffs against what it has
//!   already acted on, so a dropped request, a network blip or a mid-match
//!   restart all recover on the next send. That is what buys us no
//!   acknowledgements, no sequence numbers and no replay buffer.
//!
//! The reporter never polls game memory itself. It consumes snapshots produced
//! by the overlay's existing monitor loop (`overlay::core`), which is already
//! reading these flags every tick to draw the HUD.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use serde::{Deserialize, Serialize};

use crate::{debug_log, overlay::style::Ingest, util::time::rfc3339_millis_utc};

/// Wall-clock budget for one request, DNS through response body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Backoff schedule is `1s, 2s, 4s, …` capped at [`BACKOFF_CAP`].
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Attempts per send before giving up. The kill set is retained either way and
/// rides along on the next send, so giving up costs a delay, not data.
const MAX_ATTEMPTS: u32 = 5;
/// Upper bound on how long the worker sleeps before re-checking the stop flag.
const TICK: Duration = Duration::from_millis(200);

/// Skip reasons that are the expected result of the protocol working, not
/// something to log.
///
/// All three are the norm rather than the exception, because we deliberately
/// send the whole boss list and deliberately resend the full kill set every
/// time: most flags are not squares at all (`not_a_square`), some are squares
/// that this board did not draw (`not_on_this_board`), and everything already
/// dealt with comes back `already_fired`. Together they account for nearly
/// every entry in `skipped`, so logging them would bury the interesting lines
/// under a couple of hundred per heartbeat.
///
/// Deliberately *not* listed: `no_opponents` and `insert_failed`, which both
/// mean a kill did not land and are worth seeing.
const ROUTINE_SKIPS: [&str; 3] = ["already_fired", "not_on_this_board", "not_a_square"];

//
// ----------------------------------------------------
// Settings
// ----------------------------------------------------
//

#[derive(Debug, Clone)]
pub struct IngestSettings {
    pub url: String,
    pub token: String,
    /// Minimum gap between requests. New kills are coalesced into one send
    /// rather than firing a request per flag.
    pub interval: Duration,
    /// How often to resend the full kill set when nothing has changed.
    pub heartbeat: Duration,
}

impl IngestSettings {
    /// Builds settings from the `[ingest]` config section.
    ///
    /// Returns `None` when the section is absent or when either `url` or
    /// `token` is empty, which is what keeps the feature off for everyone who
    /// has not opted in.
    pub fn from_config(cfg: Option<&Ingest>) -> Option<Self> {
        let cfg = cfg?;

        let url = cfg.url.as_deref().unwrap_or("").trim().to_string();
        let token = cfg.token.as_deref().unwrap_or("").trim().to_string();

        if url.is_empty() || token.is_empty() {
            debug_log!("[ignite_overlay] [ingest] disabled (url or token not set)");
            return None;
        }

        // The server is the authority on token validity; a surprising shape is
        // worth a log line but not a refusal to start.
        if token.len() != 48 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
            debug_log!(
                "[ignite_overlay] [ingest] ⚠ token is not 48 hex characters ({} chars) — sending anyway",
                token.len()
            );
        }

        let interval = Duration::from_millis(cfg.interval_ms.unwrap_or(1_000).max(100));
        let heartbeat = Duration::from_secs(cfg.heartbeat_s.unwrap_or(60).max(5));

        Some(Self {
            url,
            token,
            interval,
            heartbeat,
        })
    }
}

//
// ----------------------------------------------------
// Snapshots in, status out
// ----------------------------------------------------
//

/// One observation of the tracked flags, produced by the monitor loop.
///
/// `kills` is every tracked flag currently reading true — a complete state, not
/// a delta — so dropping an intermediate snapshot is harmless.
pub struct KillSnapshot {
    pub kills: Vec<i32>,
    pub observed_at: SystemTime,
}

/// The server-computed score for the current match. Rendered verbatim; the
/// overlay derives nothing from it.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Tally {
    #[serde(default)]
    pub hits: u32,
    #[serde(default)]
    pub misses: u32,
    #[serde(default)]
    pub shots: u32,
    #[serde(default)]
    pub accuracy: i32,
}

/// What the overlay renders. Every field is set from the last response — the
/// overlay holds no tally state of its own.
#[derive(Debug, Clone, Default)]
pub struct IngestStatus {
    /// False once the server has told us `not_in_match`. The tally line is
    /// hidden in that state.
    pub in_match: bool,
    pub tally: Option<Tally>,
    /// Last send failed or was rejected for a reason other than
    /// `not_in_match`. Without this a stale tally looks identical to a live
    /// one.
    pub warn: bool,
    /// Short reason for `warn`, for the expanded overlay and logs.
    pub last_error: Option<String>,
    /// Size of the local kill set, for diagnostics.
    pub kills_tracked: usize,
}

pub type SharedIngestStatus = Arc<RwLock<IngestStatus>>;

pub fn create_status() -> SharedIngestStatus {
    Arc::new(RwLock::new(IngestStatus::default()))
}

//
// ----------------------------------------------------
// Wire format
// ----------------------------------------------------
//

#[derive(Serialize)]
struct Payload<'a> {
    token: &'a str,
    kills: Vec<WireKill>,
}

#[derive(Serialize)]
struct WireKill {
    flag: i32,
    /// First local observation of this flag being true. Optional on the wire;
    /// the server clamps it to a few seconds around arrival. It exists so a
    /// recorded kill time is not inflated by poll and network latency.
    at: String,
}

#[derive(Deserialize)]
struct IngestResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    tally: Option<Tally>,
    #[serde(default)]
    fired: Vec<FiredEntry>,
    #[serde(default)]
    skipped: Vec<SkippedEntry>,
}

// `fired` and `skipped` are deserialised purely so they can be logged, and
// `debug_log!` compiles to nothing in release builds — hence the allow.
#[derive(Deserialize)]
#[allow(dead_code)]
struct FiredEntry {
    #[serde(default)]
    flag: i64,
    #[serde(default)]
    cell: Option<i64>,
    #[serde(default)]
    result: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct SkippedEntry {
    #[serde(default)]
    flag: i64,
    #[serde(default)]
    reason: Option<String>,
}

/// Result of one HTTP attempt, classified into what the caller should do next.
enum Attempt {
    /// `ok: true`.
    Accepted(IngestResponse),
    /// `ok: false` with a reason that will not change on retry.
    Rejected(String),
    /// The player is not in a live match. The normal, common case — not a
    /// failure, and not worth a retry.
    NotInMatch,
    /// Transport failure, 5xx or 429 — worth retrying.
    Retryable(String),
}

/// Classifies an `ok: false` reply.
///
/// `not_in_match` is the normal, common case — the player simply is not in a
/// live match — and must not be treated as a failure. Everything else,
/// including anything unrecognised, is permanent: retrying `unknown_token`
/// cannot help, and treating an unexpected reply as retryable would turn a
/// server-side change into a hot loop.
fn classify_rejection(error: String) -> Attempt {
    if error == "not_in_match" {
        Attempt::NotInMatch
    } else {
        Attempt::Rejected(error)
    }
}

//
// ----------------------------------------------------
// Worker
// ----------------------------------------------------
//

/// Starts the reporter worker.
///
/// Returns the sender the monitor loop should push snapshots into, or `None` if
/// the feature is not configured — in which case the caller does no extra work
/// per tick and the mod behaves exactly as it did before.
pub fn start_reporter(
    settings: Option<IngestSettings>,
    status: SharedIngestStatus,
    stop: Arc<AtomicBool>,
) -> Option<Sender<KillSnapshot>> {
    let settings = settings?;
    let (tx, rx) = unbounded();

    debug_log!(
        "[ignite_overlay] [ingest] reporting to {} (interval {}ms, heartbeat {}s)",
        settings.url,
        settings.interval.as_millis(),
        settings.heartbeat.as_secs()
    );

    thread::spawn(move || run(settings, rx, status, stop));
    Some(tx)
}

fn run(
    settings: IngestSettings,
    rx: Receiver<KillSnapshot>,
    status: SharedIngestStatus,
    stop: Arc<AtomicBool>,
) {
    let agent = build_agent();

    // First local observation of each flag being true. Insert-only: a save
    // reload or a quit to menu makes the observed set shrink, and that is never
    // an un-kill. The server only ever adds, so retaining the flag is both
    // harmless and what lets a reloaded save keep reporting correctly.
    let mut first_seen: BTreeMap<i32, SystemTime> = BTreeMap::new();
    let mut unsent = false;
    let mut have_snapshot = false;
    let mut last_send: Option<Instant> = None;

    while !stop.load(Ordering::SeqCst) {
        // Absorb everything queued, then block briefly so the stop flag stays
        // responsive. Later snapshots supersede earlier ones.
        let mut received = false;
        loop {
            match rx.try_recv() {
                Ok(snap) => {
                    received = true;
                    have_snapshot = true;
                    unsent |= merge(&mut first_seen, snap);
                }
                Err(_) => break,
            }
        }
        if !received {
            match rx.recv_timeout(TICK) {
                Ok(snap) => {
                    have_snapshot = true;
                    unsent |= merge(&mut first_seen, snap);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    debug_log!("[ignite_overlay] [ingest] monitor disconnected — worker exiting");
                    break;
                }
            }
        }

        // Nothing is sent until the monitor has produced a snapshot, which is
        // what keeps us silent while the player is not yet in world.
        let due = match last_send {
            // First contact as soon as there is anything to report from, even
            // with an empty kill set: it populates the tally and surfaces a bad
            // token straight away rather than at the first kill of the match.
            None => have_snapshot,
            // New kills are coalesced into one request rather than one each.
            Some(t) if unsent => t.elapsed() >= settings.interval,
            Some(t) => t.elapsed() >= settings.heartbeat,
        };
        if !due {
            continue;
        }

        last_send = Some(Instant::now());
        // Cleared before sending, not after: on failure the kills stay in the
        // local set and ride along on the next heartbeat rather than retrying
        // every `interval`.
        unsent = false;

        send_with_retry(&agent, &settings, &first_seen, &status, &stop);
    }

    debug_log!("[ignite_overlay] [ingest] worker exiting");
}

/// Folds a snapshot into the kill set. Returns true if anything was newly seen.
fn merge(first_seen: &mut BTreeMap<i32, SystemTime>, snap: KillSnapshot) -> bool {
    let KillSnapshot { kills, observed_at } = snap;
    let mut new_kills = false;

    for flag in kills {
        if let Entry::Vacant(slot) = first_seen.entry(flag) {
            slot.insert(observed_at);
            new_kills = true;
        }
    }

    new_kills
}

fn build_agent() -> ureq::Agent {
    // Pooling is disabled so every report is a fresh connect/send/close. The
    // endpoint is a serverless function with a per-invocation wall-clock limit
    // while matches run over an hour, so a held-open socket would be killed
    // partway through and kills would silently stop landing.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        // 4xx/5xx must not become `Err`: the endpoint returns its JSON error
        // body with a 400 or 403, and that body is how we tell
        // `missing_token` from `not_in_match`.
        .http_status_as_error(false)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .max_idle_age(Duration::from_secs(0))
        .build();

    config.into()
}

fn send_with_retry(
    agent: &ureq::Agent,
    settings: &IngestSettings,
    first_seen: &BTreeMap<i32, SystemTime>,
    status: &SharedIngestStatus,
    stop: &Arc<AtomicBool>,
) {
    let body = match build_body(&settings.token, first_seen) {
        Ok(body) => body,
        Err(e) => {
            // Serialising our own struct should not fail; if it somehow does,
            // there is nothing to retry.
            debug_log!("[ignite_overlay] [ingest] ❌ could not serialise payload: {e}");
            set_warn(status, first_seen.len(), &format!("payload error: {e}"));
            return;
        }
    };

    let mut backoff = BACKOFF_START;

    for attempt in 1..=MAX_ATTEMPTS {
        if stop.load(Ordering::SeqCst) {
            return;
        }

        match send_once(agent, &settings.url, &body) {
            Attempt::Accepted(resp) => {
                log_accepted(&resp);
                let mut w = status.write().unwrap();
                *w = IngestStatus {
                    in_match: true,
                    tally: resp.tally,
                    warn: false,
                    last_error: None,
                    kills_tracked: first_seen.len(),
                };
                return;
            }
            Attempt::NotInMatch => {
                // Expected whenever the player simply is not playing a match.
                let mut w = status.write().unwrap();
                w.in_match = false;
                w.warn = false;
                w.last_error = None;
                w.kills_tracked = first_seen.len();
                return;
            }
            Attempt::Rejected(error) => {
                debug_log!("[ignite_overlay] [ingest] ❌ rejected: {error}");
                set_warn(status, first_seen.len(), &error);
                return;
            }
            Attempt::Retryable(error) => {
                if attempt == MAX_ATTEMPTS {
                    debug_log!(
                        "[ignite_overlay] [ingest] ❌ giving up after {attempt} attempts: {error} \
                         — kills retained for the next send"
                    );
                    set_warn(status, first_seen.len(), &error);
                    return;
                }
                debug_log!(
                    "[ignite_overlay] [ingest] ⚠ attempt {attempt} failed ({error}); retrying in {}s",
                    backoff.as_secs()
                );
                if !sleep_interruptible(backoff, stop) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_CAP);
            }
        }
    }
}

fn build_body(
    token: &str,
    first_seen: &BTreeMap<i32, SystemTime>,
) -> Result<String, serde_json::Error> {
    let kills = first_seen
        .iter()
        .map(|(&flag, &at)| WireKill {
            flag,
            at: rfc3339_millis_utc(at),
        })
        .collect();

    serde_json::to_string(&Payload { token, kills })
}

fn send_once(agent: &ureq::Agent, url: &str, body: &str) -> Attempt {
    let response = agent
        .post(url)
        .header("Content-Type", "application/json")
        // Belt and braces alongside the disabled pool: no connection is kept.
        .header("Connection", "close")
        .send(body);

    let mut response = match response {
        Ok(r) => r,
        Err(e) => return Attempt::Retryable(format!("transport: {e}")),
    };

    let status = response.status().as_u16();

    let text = match response.body_mut().read_to_string() {
        Ok(t) => t,
        Err(e) => return Attempt::Retryable(format!("read body (HTTP {status}): {e}")),
    };

    // 5xx and 429 are the server's problem and may well pass on a retry.
    if status >= 500 || status == 429 {
        return Attempt::Retryable(format!("HTTP {status}"));
    }

    let parsed: IngestResponse = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            // A proxy or captive portal can return HTML with a 200. Retrying is
            // reasonable and bounded.
            return Attempt::Retryable(format!("HTTP {status}, unparseable body: {e}"));
        }
    };

    if parsed.ok {
        return Attempt::Accepted(parsed);
    }

    classify_rejection(
        parsed
            .error
            .unwrap_or_else(|| format!("HTTP {status}, no error field")),
    )
}

fn set_warn(status: &SharedIngestStatus, kills_tracked: usize, error: &str) {
    let mut w = status.write().unwrap();
    w.warn = true;
    w.last_error = Some(error.to_string());
    w.kills_tracked = kills_tracked;
}

/// Release builds compile `debug_log!` away, leaving these loop bindings unused.
#[cfg_attr(not(debug_assertions), allow(unused_variables))]
fn log_accepted(resp: &IngestResponse) {
    for f in &resp.fired {
        debug_log!(
            "[ignite_overlay] [ingest] ✅ fired flag {} → cell {:?} ({})",
            f.flag,
            f.cell,
            f.result.as_deref().unwrap_or("?")
        );
    }
    for s in &resp.skipped {
        let reason = s.reason.as_deref().unwrap_or("?");
        if !ROUTINE_SKIPS.contains(&reason) {
            debug_log!(
                "[ignite_overlay] [ingest] skipped flag {}: {}",
                s.flag,
                reason
            );
        }
    }
}

/// Sleeps in short slices so teardown is not held up by a long backoff.
/// Returns false if the stop flag was raised.
fn sleep_interruptible(total: Duration, stop: &Arc<AtomicBool>) -> bool {
    let mut slept = Duration::ZERO;
    while slept < total {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let slice = TICK.min(total - slept);
        thread::sleep(slice);
        slept += slice;
    }
    !stop.load(Ordering::SeqCst)
}

//
// ----------------------------------------------------
// Overlay text
// ----------------------------------------------------
//

/// Renders the one-line tally, or `None` when the line should be hidden.
///
/// Everything shown comes from the last response. `Acc —` rather than `0%`
/// before the first shot, because nothing has missed yet.
pub fn tally_line(status: &IngestStatus) -> Option<String> {
    if !status.in_match {
        return None;
    }
    let tally = status.tally?;

    // ASCII only. The overlay's embedded font covers Latin characters, so a
    // warning sign or an em dash renders as a `?` box and the reassurance the
    // line exists for turns into a puzzle.
    let acc = if tally.shots == 0 {
        "-".to_string()
    } else {
        format!("{}%", tally.accuracy)
    };

    let warn = if status.warn { "   [!]" } else { "" };

    Some(format!(
        "Hit {}   Miss {}   Total {}   Acc {}{}",
        tally.hits, tally.misses, tally.shots, acc, warn
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(url: &str, token: &str) -> Option<IngestSettings> {
        IngestSettings::from_config(Some(&Ingest {
            url: Some(url.to_string()),
            token: Some(token.to_string()),
            interval_ms: None,
            heartbeat_s: None,
        }))
    }

    #[test]
    fn disabled_without_url_or_token() {
        assert!(IngestSettings::from_config(None).is_none());
        assert!(settings("", "").is_none());
        assert!(settings("https://example.test/f", "").is_none());
        assert!(settings("", "ab").is_none());
        // Whitespace-only is empty too.
        assert!(settings("   ", "   ").is_none());
    }

    #[test]
    fn enabled_with_both() {
        let s = settings("https://example.test/f", &"a".repeat(48)).unwrap();
        assert_eq!(s.url, "https://example.test/f");
        assert_eq!(s.interval, Duration::from_millis(1_000));
        assert_eq!(s.heartbeat, Duration::from_secs(60));
    }

    #[test]
    fn odd_token_still_enables() {
        // The server is the authority on validity; a short token must not
        // silently disable reporting.
        assert!(settings("https://example.test/f", "not-hex").is_some());
    }

    #[test]
    fn intervals_have_floors() {
        let s = IngestSettings::from_config(Some(&Ingest {
            url: Some("https://example.test/f".into()),
            token: Some("a".repeat(48)),
            interval_ms: Some(0),
            heartbeat_s: Some(0),
        }))
        .unwrap();
        assert_eq!(s.interval, Duration::from_millis(100));
        assert_eq!(s.heartbeat, Duration::from_secs(5));
    }

    #[test]
    fn body_carries_full_set_sorted_with_timestamps() {
        let mut seen = BTreeMap::new();
        seen.insert(31150800, SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_020));
        seen.insert(1042360800, SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_100_140));

        let body = build_body("tok", &seen).unwrap();
        assert_eq!(
            body,
            r#"{"token":"tok","kills":[{"flag":31150800,"at":"2023-11-14T22:13:20.020Z"},{"flag":1042360800,"at":"2023-11-14T22:15:00.140Z"}]}"#
        );
    }

    #[test]
    fn merge_is_insert_only_and_keeps_first_timestamp() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let mut seen = BTreeMap::new();

        assert!(merge(&mut seen, KillSnapshot { kills: vec![1, 2], observed_at: t0 }));
        // Same flags again: nothing new, timestamps unchanged.
        assert!(!merge(&mut seen, KillSnapshot { kills: vec![1, 2], observed_at: t1 }));
        assert_eq!(seen[&1], t0);

        // A save reload shrinks the observed set; that is never an un-kill.
        assert!(!merge(&mut seen, KillSnapshot { kills: vec![], observed_at: t1 }));
        assert_eq!(seen.len(), 2);

        // A genuinely new flag reports as new.
        assert!(merge(&mut seen, KillSnapshot { kills: vec![2, 3], observed_at: t1 }));
        assert_eq!(seen[&3], t1);
    }

    #[test]
    fn not_in_match_is_not_a_failure() {
        assert!(matches!(
            classify_rejection("not_in_match".into()),
            Attempt::NotInMatch
        ));
    }

    #[test]
    fn every_other_rejection_is_permanent() {
        for e in [
            "missing_token",
            "unknown_token",
            "ambiguous_match",
            "unsupported_square_set",
            // An error this build has never heard of must not be retried.
            "something_added_server_side_later",
        ] {
            assert!(
                matches!(classify_rejection(e.into()), Attempt::Rejected(_)),
                "{e} should be permanent, not retried"
            );
        }
    }

    #[test]
    fn tally_line_hidden_when_not_in_match() {
        let mut s = IngestStatus::default();
        assert_eq!(tally_line(&s), None);
        // In a match but no response yet: still nothing to render.
        s.in_match = true;
        assert_eq!(tally_line(&s), None);
    }

    #[test]
    fn tally_line_renders_response() {
        let s = IngestStatus {
            in_match: true,
            tally: Some(Tally { hits: 8, misses: 4, shots: 12, accuracy: 67 }),
            ..Default::default()
        };
        assert_eq!(tally_line(&s).unwrap(), "Hit 8   Miss 4   Total 12   Acc 67%");
    }

    #[test]
    fn tally_line_dashes_accuracy_before_first_shot() {
        let s = IngestStatus {
            in_match: true,
            tally: Some(Tally::default()),
            ..Default::default()
        };
        assert_eq!(tally_line(&s).unwrap(), "Hit 0   Miss 0   Total 0   Acc -");
    }

    #[test]
    fn tally_line_warns_when_last_send_failed() {
        let s = IngestStatus {
            in_match: true,
            tally: Some(Tally { hits: 8, misses: 4, shots: 12, accuracy: 67 }),
            warn: true,
            ..Default::default()
        };
        let line = tally_line(&s).unwrap();
        assert!(line.ends_with("[!]"), "{line}");
        // Every character must exist in the Latin-only embedded font.
        assert!(line.is_ascii(), "tally line must stay ASCII: {line}");
    }

    #[test]
    fn parses_documented_success_response() {
        let raw = r#"{"ok":true,
            "fired":[{"flag":1042360800,"cell":37,"result":"hit"}],
            "skipped":[{"flag":31150800,"reason":"already_fired"}],
            "tally":{"hits":8,"misses":4,"shots":12,"accuracy":67}}"#;
        let r: IngestResponse = serde_json::from_str(raw).unwrap();
        assert!(r.ok);
        assert_eq!(r.fired.len(), 1);
        assert_eq!(r.fired[0].cell, Some(37));
        assert_eq!(r.tally.unwrap().accuracy, 67);
    }

    #[test]
    fn parses_documented_error_response() {
        let r: IngestResponse = serde_json::from_str(r#"{"ok":false,"error":"not_in_match"}"#).unwrap();
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("not_in_match"));
        assert!(r.tally.is_none());
        assert!(r.fired.is_empty());
    }

    /// Captured verbatim from the live endpoint. Sending the whole boss list
    /// means most flags come back `not_a_square` or `not_on_this_board`, and
    /// resending the kill set means the rest come back `already_fired`; all are
    /// the protocol working, so none of them is logged.
    #[test]
    fn routine_skips_are_not_noteworthy() {
        let raw = r#"{"ok":true,"fired":[],"skipped":[
            {"flag":31150800,"reason":"already_fired"},
            {"flag":18000850,"reason":"not_a_square"},
            {"flag":1042360800,"reason":"not_on_this_board"}],
            "tally":{"hits":0,"misses":1,"shots":1,"accuracy":0}}"#;
        let r: IngestResponse = serde_json::from_str(raw).unwrap();

        assert!(r.ok);
        for s in &r.skipped {
            assert!(
                ROUTINE_SKIPS.contains(&s.reason.as_deref().unwrap()),
                "{:?} should be treated as routine",
                s.reason
            );
        }

        // A kill that did not land must stay visible.
        for noisy in ["no_opponents", "insert_failed"] {
            assert!(
                !ROUTINE_SKIPS.contains(&noisy),
                "{noisy} means a kill was lost and must be logged"
            );
        }

        // One shot taken, so accuracy renders as a number rather than a dash.
        let status = IngestStatus {
            in_match: true,
            tally: r.tally,
            ..Default::default()
        };
        assert_eq!(
            tally_line(&status).unwrap(),
            "Hit 0   Miss 1   Total 1   Acc 0%"
        );
    }

    /// A live fire, also captured verbatim from the endpoint.
    #[test]
    fn parses_live_fired_entry() {
        let raw = r#"{"ok":true,"fired":[{"flag":31150800,"cell":28,"result":"miss"}],
            "skipped":[{"flag":1042360800,"reason":"not_on_this_board"}],
            "tally":{"hits":0,"misses":1,"shots":1,"accuracy":0}}"#;
        let r: IngestResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(r.fired.len(), 1);
        assert_eq!(r.fired[0].flag, 31150800);
        assert_eq!(r.fired[0].cell, Some(28));
        assert_eq!(r.fired[0].result.as_deref(), Some("miss"));
    }

    /// The config this repo ships must parse, and must leave reporting off.
    ///
    /// Guards two regressions at once: a malformed `[ingest]` section reaching a
    /// release, and a real token being committed by accident.
    #[test]
    fn shipped_config_parses_and_leaves_ingest_off() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/ignite_overlay_config.toml");
        let raw = std::fs::read_to_string(path).expect("shipped config should exist");
        let cfg: crate::overlay::style::IgniteConfig =
            toml::from_str(&raw).expect("shipped config should deserialise");

        let ingest = cfg.ingest.as_ref().expect("[ingest] section should be present");
        assert!(
            !ingest.url.as_deref().unwrap_or("").is_empty(),
            "the endpoint url should ship filled in, so only a token is needed"
        );
        assert!(
            ingest.token.as_deref().unwrap_or("").is_empty(),
            "the shipped config must NOT contain a token"
        );
        assert!(
            IngestSettings::from_config(cfg.ingest.as_ref()).is_none(),
            "reporting must be off in the shipped config"
        );
    }

    /// Exercises the real network path — ureq, TLS, serialisation and response
    /// classification — end to end. Ignored by default because it needs a
    /// network and a token; run it with:
    ///
    /// ```text
    /// $env:ER_OVERLAY_INGEST_URL   = "https://.../auto-fire"
    /// $env:ER_OVERLAY_INGEST_TOKEN = "<48 hex chars>"
    /// cargo test --lib -- --ignored --nocapture live_round_trip
    /// ```
    ///
    /// Safe to run mid-match: an empty kill set cannot fire anything.
    #[test]
    #[ignore = "needs ER_OVERLAY_INGEST_URL, ER_OVERLAY_INGEST_TOKEN and network"]
    fn live_round_trip() {
        let (Ok(url), Ok(token)) = (
            std::env::var("ER_OVERLAY_INGEST_URL"),
            std::env::var("ER_OVERLAY_INGEST_TOKEN"),
        ) else {
            panic!("set ER_OVERLAY_INGEST_URL and ER_OVERLAY_INGEST_TOKEN");
        };

        let agent = build_agent();
        let empty = BTreeMap::new();

        match send_once(&agent, &url, &build_body(&token, &empty).unwrap()) {
            Attempt::Accepted(r) => println!("accepted, tally = {:?}", r.tally),
            Attempt::NotInMatch => println!("not_in_match (expected while idle)"),
            Attempt::Rejected(e) => panic!("unexpectedly rejected: {e}"),
            Attempt::Retryable(e) => panic!("transport failure: {e}"),
        }

        // A well-formed but unknown token must come back as a permanent
        // rejection rather than something we would sit and retry.
        let bogus = build_body(&"0".repeat(48), &empty).unwrap();
        match send_once(&agent, &url, &bogus) {
            Attempt::Rejected(e) => assert_eq!(e, "unknown_token"),
            Attempt::Accepted(_) => panic!("an unknown token was accepted"),
            Attempt::NotInMatch => panic!("an unknown token returned not_in_match"),
            Attempt::Retryable(e) => panic!("expected a permanent rejection, got retryable: {e}"),
        }
    }

    #[test]
    fn tolerates_missing_and_null_fields() {
        // A trimmed-down or partially-null reply must not fail parsing.
        let r: IngestResponse =
            serde_json::from_str(r#"{"ok":true,"tally":{"hits":3},"fired":[{"flag":1}]}"#).unwrap();
        let t = r.tally.unwrap();
        assert_eq!((t.hits, t.misses, t.shots, t.accuracy), (3, 0, 0, 0));
        assert_eq!(r.fired[0].cell, None);

        let r: IngestResponse = serde_json::from_str(r#"{}"#).unwrap();
        assert!(!r.ok);
    }
}
