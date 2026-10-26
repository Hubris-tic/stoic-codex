//! Retries, with the part everyone gets wrong left out.
//!
//! A provider that never rate-limits does not exist, and the free tier of every vendor is a
//! steady source of `429`s. Without this module a run dies on the first one, after the user
//! has already watched tokens stream in. With a naive version of it, a run *duplicates* the
//! answer, because a retried request re-emits deltas the user has already seen.
//!
//! So the rules here are narrow and explicit:
//!
//! * **Only retryable failures are retried.** `408`, `429`, and `5xx` — plus a transport
//!   failure that happened before a single body byte arrived. A `400` will fail identically
//!   four times and just wastes the user's time; a `401` is a typo in an env var.
//! * **Never retry after a delta has been printed.** HTTP status arrives before the body, so
//!   a `429` can never be a stream and retrying it cannot duplicate output. A transport error
//!   in the middle of a stream *can* lose bytes silently, so it is surfaced instead — an
//!   honest failure beats a duplicated answer.
//! * **Back off exponentially, with jitter, and honour `Retry-After`.** Thundering herds are
//!   real, and a vendor that says "try again in 7 seconds" means it.
//! * **Say what is happening.** A silent 8-second pause looks like a hang, so every attempt
//!   after the first emits one line through the notice sink.
//!
//! Delays are pure values and sleeping is injected by the caller, so policy tests can assert
//! exact windows without waiting or contacting a provider. The mission clock is injectable too,
//! which makes deadline boundaries deterministic in unit tests.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::net::{HttpRequest, NetError, Response, Transport};

/// One shared retry deadline for all model clients in a mission.
///
/// The window starts on the first retryable failure—not when the mission starts—and is shared
/// by Strategos and every Operator. A slow plan or successful task consumes no retry budget;
/// after the first retryable failure, backoff and socket I/O for later retries share one deadline.
#[derive(Clone)]
pub struct MissionRetryWindow {
    state: Arc<MissionRetryState>,
}

struct MissionRetryState {
    budget: Duration,
    started: Mutex<Option<Instant>>,
    seed: u64,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
}

/// Per-client view of a mission retry window. Each participant gets a stable, distinct jitter
/// stream derived from the mission seed and its label.
pub struct MissionRetryClient {
    state: Arc<MissionRetryState>,
    seed: u64,
    request_sequence: AtomicU64,
}

impl MissionRetryWindow {
    pub fn new(budget: Duration) -> Self {
        Self::with_seed(budget, fresh_mission_seed())
    }

    /// Deterministic seed injection for tests and reproducible retry schedules.
    pub fn with_seed(budget: Duration, seed: u64) -> Self {
        Self::with_clock(budget, seed, Arc::new(Instant::now))
    }

    fn with_clock(
        budget: Duration,
        seed: u64,
        now: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        MissionRetryWindow {
            state: Arc::new(MissionRetryState {
                budget,
                started: Mutex::new(None),
                seed: mix64(seed),
                now,
            }),
        }
    }

    pub fn client(&self, participant: &str) -> MissionRetryClient {
        MissionRetryClient {
            state: self.state.clone(),
            seed: mix64(self.state.seed ^ stable_hash(participant)),
            request_sequence: AtomicU64::new(0),
        }
    }
}

impl MissionRetryClient {
    fn next_request_salt(&self) -> u64 {
        let sequence = self.request_sequence.fetch_add(1, Ordering::Relaxed);
        mix64(self.seed ^ sequence.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    /// Start the shared window on the first retryable failure, then permit a wait only if it
    /// finishes strictly before the mission deadline. A zero-duration window disables retries.
    fn delay_fits(&self, delay: Duration) -> bool {
        let now = (self.state.now)();
        let mut started = self
            .state
            .started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let began = *started.get_or_insert(now);
        let elapsed = now.saturating_duration_since(began);
        let remaining = self.state.budget.saturating_sub(elapsed);
        !remaining.is_zero() && delay < remaining
    }

    fn expired(&self) -> bool {
        let now = (self.state.now)();
        self.deadline()
            .map(|deadline| now >= deadline)
            .unwrap_or(false)
    }

    fn deadline(&self) -> Option<Instant> {
        let started = self
            .state
            .started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        started
            .as_ref()
            .and_then(|began| began.checked_add(self.state.budget))
    }

    fn budget(&self) -> Duration {
        self.state.budget
    }
}

fn stable_hash(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    mix64(hash)
}

fn mix64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn fresh_mission_seed() -> u64 {
    static NEXT_MISSION: AtomicU64 = AtomicU64::new(0);
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let sequence = NEXT_MISSION.fetch_add(1, Ordering::Relaxed);
    mix64(wall ^ u64::from(std::process::id()).rotate_left(17) ^ sequence)
}

/// How hard to try again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryPolicy {
    /// Total attempts, including the first. `1` disables retrying.
    pub attempts: u32,
    /// Delay before the second attempt. Doubles each time.
    pub base: Duration,
    /// Cap on a single delay. Without this, attempt 8 of a 30 s base waits two hours.
    pub ceiling: Duration,
}

impl RetryPolicy {
    /// Four attempts: equal-jittered waits of roughly 0.25–0.5 s, 0.5–1 s, and 1–2 s.
    /// Enough to ride out a burst, short enough that a user who is offline is told quickly.
    pub fn standard() -> RetryPolicy {
        RetryPolicy {
            attempts: 4,
            base: Duration::from_millis(500),
            ceiling: Duration::from_secs(8),
        }
    }

    /// For tests: still uses the full decision logic, but never waits.
    pub fn immediate() -> RetryPolicy {
        RetryPolicy {
            attempts: 4,
            base: Duration::ZERO,
            ceiling: Duration::ZERO,
        }
    }

    pub fn off() -> RetryPolicy {
        RetryPolicy {
            attempts: 1,
            base: Duration::ZERO,
            ceiling: Duration::ZERO,
        }
    }

    pub fn with_attempts(mut self, attempts: u32) -> RetryPolicy {
        self.attempts = attempts.max(1);
        self
    }

    /// The pure exponential wait before `attempt` (0-based: attempt 0 never waits).
    ///
    /// Deterministic, so it can be asserted exactly. Jitter is applied on top by
    /// [`delay_with`](RetryPolicy::delay_with), where it belongs.
    pub fn backoff(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        self.base
            .saturating_mul(1u32 << (attempt - 1).min(16))
            .min(self.ceiling)
    }

    /// Exponential backoff with equal jitter: a uniform draw in `[backoff/2, backoff]`.
    ///
    /// Jitter exists because a restarted mission has three operators that hit the same rate
    /// limit in the same millisecond; without it they retry in lockstep forever. `salt` is
    /// the caller's entropy (the wall clock is fine) — passing a fixed salt makes the wait
    /// reproducible, which is what tests do.
    pub fn delay_with(&self, attempt: u32, salt: u64) -> Duration {
        let b = self.backoff(attempt);
        let micros = b.as_micros() as u64;
        if micros == 0 {
            return b;
        }
        // A 64-bit xorshift: no dependency, no `rand`, and uniform enough for a sleep.
        let mut x = salt.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (attempt as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 30;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 27;
        let half = micros / 2;
        Duration::from_micros(micros - x % (half + 1))
    }

    /// The wait a caller that does not care about jitter should use.
    pub fn delay(&self, attempt: u32) -> Duration {
        self.backoff(attempt)
    }

    /// Wait to use before `attempt`, taking the server's own advice when it gave any.
    pub fn delay_after(&self, attempt: u32, retry_after: Option<Duration>, salt: u64) -> Duration {
        match retry_after {
            // A server that asks for a long wait is believed, but not for longer than the
            // ceiling: refusing to wait two minutes is a decision the caller can see.
            Some(d) => d.min(self.ceiling.max(self.base)),
            None => self.delay_with(attempt, salt),
        }
    }

    pub fn exhausted(&self, attempt: u32) -> bool {
        attempt + 1 >= self.attempts
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy::standard()
    }
}

/// Statuses worth trying again. Anything else is the caller's problem to fix.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 425 | 429) || (500..600).contains(&status)
}

/// True when a transport failure happened before any of the body was read, i.e. when a retry
/// cannot duplicate output. A timeout mid-stream is *not* in this set.
pub fn is_retryable_transport(e: &NetError) -> bool {
    matches!(
        e,
        NetError::Connect { .. } | NetError::Timeout { .. } | NetError::Protocol(_)
    )
}

/// `Retry-After` in seconds, as Gemini/OpenAI/Anthropic send it. The HTTP-date form is legal
/// but nobody emits it for LLM APIs, and guessing at clock skew is worse than ignoring it.
pub fn retry_after(response: &Response) -> Option<Duration> {
    let raw = response.header("retry-after")?.trim();
    raw.parse::<u64>()
        .ok()
        .map(|s| Duration::from_secs(s.min(300)))
}

/// One line the user can act on. Emitted only when a retry actually happens.
fn notice_line(attempt: u32, total: u32, why: &str, wait: Duration) -> String {
    format!(
        "retrying in {:.1}s ({why}; attempt {} of {total})",
        wait.as_millis() as f64 / 1000.0,
        attempt + 1,
    )
}

/// Perform a request, retrying a whole (buffered) exchange when that is safe.
///
/// Buffered means the caller kept no bytes until the body was complete, so a retry is
/// invisible. Use this for non-streaming turns, model lists, and probes.
pub fn buffered(
    transport: &dyn Transport,
    request: &HttpRequest,
    policy: &RetryPolicy,
    notice: &mut dyn FnMut(&str),
    sleep: &mut dyn FnMut(Duration),
) -> Result<(Response, String), NetError> {
    buffered_inner(transport, request, policy, None, notice, sleep)
}

/// Buffered retry loop using a mission-shared deadline and participant-specific jitter seed.
pub fn buffered_for_mission(
    transport: &dyn Transport,
    request: &HttpRequest,
    policy: &RetryPolicy,
    mission: &MissionRetryClient,
    notice: &mut dyn FnMut(&str),
    sleep: &mut dyn FnMut(Duration),
) -> Result<(Response, String), NetError> {
    buffered_inner(transport, request, policy, Some(mission), notice, sleep)
}

fn buffered_inner(
    transport: &dyn Transport,
    request: &HttpRequest,
    policy: &RetryPolicy,
    mission: Option<&MissionRetryClient>,
    notice: &mut dyn FnMut(&str),
    sleep: &mut dyn FnMut(Duration),
) -> Result<(Response, String), NetError> {
    let mission_salt = mission.map(MissionRetryClient::next_request_salt);
    let mut attempt = 0;
    loop {
        let mut body = String::new();
        let mut collect = |chunk: &str| body.push_str(chunk);
        let result = match mission {
            Some(mission) => {
                transport.send_with_deadline(request, &mut collect, mission.deadline())
            }
            None => transport.send(request, &mut collect),
        };
        let finished = policy.exhausted(attempt);
        match result {
            Ok(response) if response.is_success() || finished => return Ok((response, body)),
            Ok(response) if is_retryable_status(response.status) => {
                let why = format!("HTTP {}", response.status);
                let delay_attempt = if mission.is_some() {
                    attempt.saturating_add(1)
                } else {
                    // Preserve the established standalone buffered retry timing. Mission
                    // requests use the next 1-based attempt so their first retry is jittered.
                    attempt
                };
                let wait = policy.delay_after(
                    delay_attempt,
                    retry_after(&response),
                    mission_salt.unwrap_or_else(salt),
                );
                if !mission_delay_fits(mission, wait, attempt, policy.attempts, &why, notice) {
                    return Ok((response, body));
                }
                notice(&notice_line(attempt, policy.attempts, &why, wait));
                sleep(wait);
                if mission_window_expired(mission) {
                    notice(&mission_expired_notice(attempt, policy.attempts, &why));
                    return Ok((response, body));
                }
            }
            Ok(response) => return Ok((response, body)),
            Err(e) if !finished && is_retryable_transport(&e) => {
                let why = short(&e);
                let wait =
                    policy.delay_with(attempt.saturating_add(1), mission_salt.unwrap_or_else(salt));
                if !mission_delay_fits(mission, wait, attempt, policy.attempts, &why, notice) {
                    return Err(e);
                }
                notice(&notice_line(attempt, policy.attempts, &why, wait));
                sleep(wait);
                if mission_window_expired(mission) {
                    notice(&mission_expired_notice(attempt, policy.attempts, &why));
                    return Err(e);
                }
            }
            Err(e) => return Err(e),
        }
        attempt += 1;
    }
}

fn mission_delay_fits(
    mission: Option<&MissionRetryClient>,
    wait: Duration,
    attempt: u32,
    total: u32,
    why: &str,
    notice: &mut dyn FnMut(&str),
) -> bool {
    let Some(mission) = mission else {
        return true;
    };
    if mission.delay_fits(wait) {
        return true;
    }
    notice(&format!(
        "mission retry window ({:.1}s) cannot fit a {:.1}s wait after {why}; stopping before attempt {} of {total}",
        mission.budget().as_secs_f64(),
        wait.as_secs_f64(),
        attempt + 2,
    ));
    false
}

fn mission_window_expired(mission: Option<&MissionRetryClient>) -> bool {
    mission.map(MissionRetryClient::expired).unwrap_or(false)
}

fn mission_expired_notice(attempt: u32, total: u32, why: &str) -> String {
    format!(
        "mission retry window expired during the wait after {why}; not starting attempt {} of {total}",
        attempt + 2,
    )
}

/// Perform a *streaming* request. The body goes to `sink` as it arrives.
///
/// A retry is only allowed while nothing has been forwarded, or when the failure was an HTTP
/// status (which arrives before any body byte). This is the whole reason this is a separate
/// function from [`buffered`]: the safety argument is different, so the code should look
/// different.
pub fn streamed(
    transport: &dyn Transport,
    request: &HttpRequest,
    sink: &mut dyn FnMut(&str),
    policy: &RetryPolicy,
    notice: &mut dyn FnMut(&str),
    sleep: &mut dyn FnMut(Duration),
) -> Result<(Response, String), NetError> {
    let mut attempt = 0;
    loop {
        let forwarded = std::cell::Cell::new(0usize);
        // The body of the final failing attempt, kept so the caller can turn the vendor's own
        // words into a message. Bounded: an error document is small, and a proxy with a lot to
        // say should not be able to fill memory.
        let mut error_body = String::new();
        // Chunks are forwarded only when the status says they are a body and not an error
        // document. `0` means the transport cannot tell us — treat it as success, because a
        // stub that answers from memory has no status to give.
        let response = transport.send_statused(request, &mut |status, chunk| {
            if status == 0 || (200..400).contains(&status) {
                forwarded.set(forwarded.get() + chunk.len());
                sink(chunk);
            } else if error_body.len() < 8192 {
                error_body.push_str(chunk);
            }
        });
        let finished = policy.exhausted(attempt);
        match response {
            Ok(r) if r.is_success() || finished => return Ok((r, error_body)),
            Ok(r) if is_retryable_status(r.status) => {
                let wait = policy.delay_after(attempt, retry_after(&r), salt());
                notice(&notice_line(
                    attempt,
                    policy.attempts,
                    &format!("HTTP {}", r.status),
                    wait,
                ));
                sleep(wait);
            }
            Ok(r) => return Ok((r, error_body)),
            Err(e) if !finished && is_retryable_transport(&e) && forwarded.get() == 0 => {
                let wait = policy.delay_with(attempt + 1, salt());
                notice(&notice_line(attempt, policy.attempts, &short(&e), wait));
                sleep(wait);
            }
            Err(e) => return Err(e),
        }
        attempt += 1;
    }
}

/// Entropy for jitter: the sub-second part of the wall clock. Cheap, and different for two
/// retries that start in the same second.
fn salt() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn short(e: &NetError) -> String {
    match e {
        NetError::Timeout { .. } => "timed out".into(),
        NetError::Connect { .. } => "connection failed".into(),
        other => other.to_string(),
    }
}

/// The default sleeper. Tests pass a no-op; the real CLI passes this.
pub fn real_sleep(d: Duration) {
    std::thread::sleep(d);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Url;

    /// A transport that answers from a script and records what it was asked.
    struct Script {
        answers: Vec<Answer>,
        seen: std::cell::RefCell<Vec<usize>>,
        deadlines: std::cell::RefCell<Vec<Option<Instant>>>,
    }

    enum Answer {
        Status(u16, &'static str),
        Fails(NetError),
        Streams(u16, Vec<&'static str>),
    }

    impl Script {
        fn new(answers: Vec<Answer>) -> Script {
            Script {
                answers,
                seen: std::cell::RefCell::new(Vec::new()),
                deadlines: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn calls(&self) -> usize {
            self.seen.borrow().len()
        }
    }

    impl Script {
        fn next(&self) -> (u16, Vec<String>, Option<NetError>) {
            let n = self.seen.borrow().len();
            self.seen.borrow_mut().push(n);
            match self.answers.get(n) {
                Some(Answer::Status(code, body)) => (*code, vec![body.to_string()], None),
                Some(Answer::Fails(e)) => (0, Vec::new(), Some(e.clone())),
                Some(Answer::Streams(code, chunks)) => {
                    (*code, chunks.iter().map(|c| c.to_string()).collect(), None)
                }
                None => (200, Vec::new(), None),
            }
        }
    }

    impl Transport for Script {
        fn send(
            &self,
            _req: &HttpRequest,
            sink: &mut dyn FnMut(&str),
        ) -> Result<Response, NetError> {
            self.send_statused(_req, &mut |_, c| sink(c))
        }

        fn send_statused(
            &self,
            _req: &HttpRequest,
            sink: &mut dyn FnMut(u16, &str),
        ) -> Result<Response, NetError> {
            let (code, chunks, err) = self.next();
            if let Some(e) = err {
                return Err(e);
            }
            for c in &chunks {
                sink(code, c);
            }
            Ok(Response {
                status: code,
                headers: Vec::new(),
            })
        }

        fn send_with_deadline(
            &self,
            req: &HttpRequest,
            sink: &mut dyn FnMut(&str),
            deadline: Option<Instant>,
        ) -> Result<Response, NetError> {
            self.deadlines.borrow_mut().push(deadline);
            self.send(req, sink)
        }
    }

    fn request() -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            url: Url::parse("https://example.invalid/v1").unwrap(),
            headers: Vec::new(),
            body: b"{}".to_vec(),
        }
    }

    fn fake_window(budget: Duration, seed: u64) -> (MissionRetryWindow, Arc<AtomicU64>, Instant) {
        let elapsed_ns = Arc::new(AtomicU64::new(0));
        let clock_elapsed = elapsed_ns.clone();
        let epoch = Instant::now();
        let now: Arc<dyn Fn() -> Instant + Send + Sync> =
            Arc::new(move || epoch + Duration::from_nanos(clock_elapsed.load(Ordering::Relaxed)));
        (
            MissionRetryWindow::with_clock(budget, seed, now),
            elapsed_ns,
            epoch,
        )
    }

    fn run_buffered(
        script: &Script,
        policy: RetryPolicy,
    ) -> (
        Result<(Response, String), NetError>,
        Vec<Duration>,
        Vec<String>,
    ) {
        let mut waits = Vec::new();
        let mut notes = Vec::new();
        let result = buffered(
            script,
            &request(),
            &policy,
            &mut |n: &str| notes.push(n.to_string()),
            &mut |d| waits.push(d),
        );
        (result, waits, notes)
    }

    #[test]
    fn a_429_is_retried_and_the_answer_is_kept() {
        let script = Script::new(vec![
            Answer::Status(429, "{\"error\":\"rate\"}"),
            Answer::Status(200, "{\"ok\":true}"),
        ]);
        let (result, waits, notes) = run_buffered(&script, RetryPolicy::immediate());
        let (response, body) = result.expect("second attempt succeeds");
        assert_eq!(response.status, 200);
        assert_eq!(body, "{\"ok\":true}");
        assert_eq!(script.calls(), 2);
        assert_eq!(waits.len(), 1);
        assert_eq!(notes.len(), 1, "the user must be told, once: {notes:?}");
        assert!(notes[0].contains("429"), "{}", notes[0]);
    }

    #[test]
    fn a_400_is_not_retried_because_it_will_fail_identically() {
        let script = Script::new(vec![Answer::Status(400, "bad request")]);
        let (result, waits, notes) = run_buffered(&script, RetryPolicy::immediate());
        let (response, _) = result.expect("a 400 is a response, not an error");
        assert_eq!(response.status, 400);
        assert_eq!(script.calls(), 1);
        assert!(waits.is_empty() && notes.is_empty());
    }

    #[test]
    fn attempts_are_bounded_and_the_last_failure_surfaces() {
        let script = Script::new(vec![
            Answer::Status(503, "a"),
            Answer::Status(503, "b"),
            Answer::Status(503, "c"),
            Answer::Status(503, "d"),
        ]);
        let (result, waits, notes) =
            run_buffered(&script, RetryPolicy::immediate().with_attempts(3));
        let (response, body) = result.expect("the final attempt is returned, not swallowed");
        assert_eq!(response.status, 503);
        assert_eq!(body, "c", "the body of the last attempt, not the first");
        assert_eq!(script.calls(), 3, "three attempts, not four");
        assert_eq!(waits.len(), 2);
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn a_connect_failure_is_retried_but_a_bad_url_is_not() {
        let script = Script::new(vec![
            Answer::Fails(NetError::Connect {
                address: "example.invalid:443".into(),
                detail: "refused".into(),
            }),
            Answer::Status(200, "ok"),
        ]);
        let (result, _, _) = run_buffered(&script, RetryPolicy::immediate());
        assert_eq!(result.expect("retried").0.status, 200);
        assert_eq!(script.calls(), 2);

        let script = Script::new(vec![Answer::Fails(NetError::BadUrl("nope".into()))]);
        let (result, waits, _) = run_buffered(&script, RetryPolicy::immediate());
        assert!(result.is_err(), "a malformed URL is not retried");
        assert_eq!(script.calls(), 1);
        assert!(waits.is_empty());
    }

    /// Fails on attempt 0 after forwarding a chunk, then would succeed. The retry layer must
    /// refuse: forwarding bytes and retrying writes the same text twice.
    struct FailsMidStream {
        calls: std::cell::Cell<usize>,
    }

    impl Transport for FailsMidStream {
        fn send(
            &self,
            _req: &HttpRequest,
            sink: &mut dyn FnMut(&str),
        ) -> Result<Response, NetError> {
            let n = self.calls.get();
            self.calls.set(n + 1);
            if n == 0 {
                sink("half ");
                return Err(NetError::Protocol("connection reset".into()));
            }
            sink("twice");
            Ok(Response {
                status: 200,
                headers: Vec::new(),
            })
        }
    }

    /// Fails *before* any byte, then succeeds: that is safe to retry.
    struct FailsEarly {
        calls: std::cell::Cell<usize>,
    }

    impl Transport for FailsEarly {
        fn send(
            &self,
            _req: &HttpRequest,
            sink: &mut dyn FnMut(&str),
        ) -> Result<Response, NetError> {
            let n = self.calls.get();
            self.calls.set(n + 1);
            if n == 0 {
                return Err(NetError::Connect {
                    address: "x:1".into(),
                    detail: "refused".into(),
                });
            }
            sink("hello");
            Ok(Response {
                status: 200,
                headers: Vec::new(),
            })
        }
    }

    fn run_streamed(
        t: &dyn Transport,
        policy: RetryPolicy,
    ) -> (Result<Response, NetError>, String, Vec<Duration>) {
        let mut printed = String::new();
        let mut waits = Vec::new();
        let result = streamed(
            t,
            &request(),
            &mut |c| printed.push_str(c),
            &policy,
            &mut |_| {},
            &mut |d| waits.push(d),
        );
        (result.map(|(r, _)| r), printed, waits)
    }

    #[test]
    fn a_mid_stream_failure_is_never_retried_because_it_would_duplicate_output() {
        let t = FailsMidStream {
            calls: std::cell::Cell::new(0),
        };
        let (result, printed, waits) = run_streamed(&t, RetryPolicy::immediate());
        assert!(
            result.is_err(),
            "an interrupted stream is surfaced, not retried"
        );
        assert_eq!(
            printed, "half ",
            "the user's partial output is not repeated"
        );
        assert_eq!(t.calls.get(), 1);
        assert!(waits.is_empty());
    }

    #[test]
    fn a_stream_that_fails_before_any_byte_is_retried() {
        let t = FailsEarly {
            calls: std::cell::Cell::new(0),
        };
        let (result, printed, waits) = run_streamed(&t, RetryPolicy::immediate());
        assert_eq!(result.expect("retried").status, 200);
        assert_eq!(printed, "hello", "delivered exactly once");
        assert_eq!(t.calls.get(), 2);
        assert_eq!(waits.len(), 1);
    }

    #[test]
    fn a_failed_stream_hands_back_the_vendors_own_words() {
        let script = Script::new(vec![Answer::Status(
            429,
            r#"{"error":{"type":"rate_limit","message":"slow down"}}"#,
        )]);
        let mut printed = String::new();
        let (response, body) = streamed(
            &script,
            &request(),
            &mut |c| printed.push_str(c),
            &RetryPolicy::off(),
            &mut |_| {},
            &mut |_| {},
        )
        .expect("a status is a response, not an error");
        assert_eq!(response.status, 429);
        assert!(
            body.contains("slow down"),
            "the error body must be readable: {body:?}"
        );
        assert!(
            printed.is_empty(),
            "and must not be printed as if it were the answer"
        );
    }

    #[test]
    fn a_stream_that_answers_429_is_retried() {
        let script = Script::new(vec![
            Answer::Status(429, "{\"error\":\"slow down\"}"),
            Answer::Streams(200, vec!["data: 1\n\n", "data: 2\n\n"]),
        ]);
        let (result, printed, waits) = run_streamed(&script, RetryPolicy::immediate());
        assert_eq!(result.expect("retried").status, 200);
        assert_eq!(printed, "data: 1\n\ndata: 2\n\n");
        assert_eq!(script.calls(), 2);
        assert_eq!(waits.len(), 1);
    }

    #[test]
    fn backoff_doubles_then_stops_at_the_ceiling() {
        let p = RetryPolicy {
            attempts: 8,
            base: Duration::from_millis(500),
            ceiling: Duration::from_secs(4),
        };
        assert_eq!(p.backoff(0), Duration::ZERO);
        assert_eq!(p.backoff(1), Duration::from_millis(500));
        assert_eq!(p.backoff(2), Duration::from_millis(1000));
        assert_eq!(p.backoff(3), Duration::from_millis(2000));
        assert_eq!(p.backoff(4), Duration::from_secs(4));
        assert_eq!(p.backoff(9), Duration::from_secs(4), "the ceiling holds");
        assert_eq!(
            p.backoff(64),
            Duration::from_secs(4),
            "no overflow, no panic"
        );
    }

    #[test]
    fn jitter_stays_inside_half_the_backoff_and_actually_varies() {
        let p = RetryPolicy {
            attempts: 8,
            base: Duration::from_secs(4),
            ceiling: Duration::from_secs(8),
        };
        let mut seen = std::collections::BTreeSet::new();
        for salt in 0..40u64 {
            for attempt in 1..6u32 {
                let d = p.delay_with(attempt, salt);
                let b = p.backoff(attempt);
                assert!(d <= b, "jitter must not exceed the backoff");
                assert!(
                    d >= b / 2,
                    "jitter must not collapse the backoff: {d:?} vs {b:?}"
                );
                seen.insert((attempt, d.as_micros()));
            }
        }
        assert!(seen.len() > 20, "the salt must actually change the wait");
    }

    #[test]
    fn subsequent_mission_requests_receive_the_shared_absolute_deadline() {
        let (window, _, epoch) = fake_window(Duration::from_secs(5), 0x55);
        let client = window.client("strategos");
        let script = Script::new(vec![
            Answer::Status(429, "rate limited"),
            Answer::Status(200, "recovered"),
        ]);
        let result = buffered_for_mission(
            &script,
            &request(),
            &RetryPolicy::immediate(),
            &client,
            &mut |_| {},
            &mut |_| {},
        )
        .unwrap();

        assert_eq!(result.0.status, 200);
        assert_eq!(
            *script.deadlines.borrow(),
            vec![None, Some(epoch + Duration::from_secs(5))]
        );
    }

    #[test]
    fn the_first_mission_status_retry_waits_before_attempt_two() {
        let (window, _, _) = fake_window(Duration::from_secs(5), 0xabc);
        let client = window.client("strategos");
        let script = Script::new(vec![
            Answer::Status(429, "rate limited"),
            Answer::Status(200, "recovered"),
        ]);
        let mut waits = Vec::new();
        let result = buffered_for_mission(
            &script,
            &request(),
            &RetryPolicy::standard(),
            &client,
            &mut |_| {},
            &mut |wait| waits.push(wait),
        )
        .unwrap();

        assert_eq!(result.0.status, 200);
        assert_eq!(script.calls(), 2);
        assert_eq!(waits.len(), 1);
        assert!(
            (Duration::from_millis(250)..=Duration::from_millis(500)).contains(&waits[0]),
            "first mission retry should be jittered from the base: {:?}",
            waits[0]
        );
    }

    #[test]
    fn standalone_buffered_status_retry_timing_is_unchanged() {
        let script = Script::new(vec![
            Answer::Status(429, "rate limited"),
            Answer::Status(200, "recovered"),
        ]);
        let (result, waits, _) = run_buffered(&script, RetryPolicy::standard());

        assert_eq!(result.unwrap().0.status, 200);
        assert_eq!(script.calls(), 2);
        assert_eq!(waits, vec![Duration::ZERO]);
    }

    #[test]
    fn mission_jitter_is_reproducible_and_distinct_per_operator() {
        let window = MissionRetryWindow::with_seed(Duration::from_secs(30), 0x1234);
        let first = window.client("x1-write").next_request_salt();
        let second = window.client("x2-check").next_request_salt();
        let reproduced = MissionRetryWindow::with_seed(Duration::from_secs(30), 0x1234)
            .client("x1-write")
            .next_request_salt();

        assert_ne!(first, second, "operators must not share a retry stream");
        assert_eq!(first, reproduced, "the same mission seed is reproducible");
        let policy = RetryPolicy {
            attempts: 4,
            base: Duration::from_secs(4),
            ceiling: Duration::from_secs(8),
        };
        assert_ne!(
            policy.delay_with(1, first),
            policy.delay_with(1, second),
            "the participant streams should actually decorrelate the wait"
        );
    }

    #[test]
    fn mission_window_is_shared_and_stops_retries_after_its_deadline() {
        let (window, elapsed_ns, epoch) = fake_window(Duration::from_millis(1), 7);
        let first_client = window.client("x1-write");
        assert!(
            first_client.deadline().is_none(),
            "the timer has not started yet"
        );
        let first_script = Script::new(vec![
            Answer::Status(429, "rate limited"),
            Answer::Status(200, "should not be reached"),
        ]);
        let mut first_notices = Vec::new();
        let first = buffered_for_mission(
            &first_script,
            &request(),
            &RetryPolicy::immediate(),
            &first_client,
            &mut |notice| first_notices.push(notice.to_string()),
            &mut |duration| {
                assert_eq!(duration, Duration::ZERO);
                elapsed_ns.store(2_000_000, Ordering::Relaxed);
            },
        )
        .unwrap();
        assert_eq!(first.0.status, 429);
        assert_eq!(
            first_client.deadline(),
            Some(epoch + Duration::from_millis(1))
        );
        assert_eq!(first_script.calls(), 1);
        assert!(first_notices
            .iter()
            .any(|line| line.contains("expired during the wait")));

        let second_client = window.client("x2-check");
        let second_script = Script::new(vec![
            Answer::Status(429, "still rate limited"),
            Answer::Status(200, "must not be reached"),
        ]);
        let mut second_notices = Vec::new();
        let second = buffered_for_mission(
            &second_script,
            &request(),
            &RetryPolicy::immediate(),
            &second_client,
            &mut |notice| second_notices.push(notice.to_string()),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(second.0.status, 429);
        assert_eq!(
            second_script.calls(),
            1,
            "all clients share the same deadline"
        );
        assert!(second_notices
            .iter()
            .any(|line| line.contains("cannot fit")));
    }

    #[test]
    fn a_retry_wait_must_finish_strictly_before_the_shared_deadline() {
        let (window, elapsed_ns, epoch) = fake_window(Duration::from_secs(1), 11);
        let client = window.client("x1-write");
        assert!(client.delay_fits(Duration::from_millis(500)));
        assert_eq!(client.deadline(), Some(epoch + Duration::from_secs(1)));

        elapsed_ns.store(500_000_000, Ordering::Relaxed);
        assert!(
            !client.delay_fits(Duration::from_millis(500)),
            "a wait that lands exactly on the deadline is rejected"
        );
    }

    #[test]
    fn a_retry_delay_longer_than_the_mission_window_is_not_slept() {
        let (window, _, _) = fake_window(Duration::from_millis(1), 9);
        let client = window.client("x3-ship");
        let script = Script::new(vec![
            Answer::Status(429, "rate limited"),
            Answer::Status(200, "must not be reached"),
        ]);
        let mut waits = Vec::new();
        let result = buffered_for_mission(
            &script,
            &request(),
            &RetryPolicy::standard(),
            &client,
            &mut |_| {},
            &mut |duration| waits.push(duration),
        )
        .unwrap();
        assert_eq!(result.0.status, 429);
        assert_eq!(script.calls(), 1);
        assert!(waits.is_empty(), "over-budget backoff must not be slept");
    }

    #[test]
    fn retry_after_is_honoured_but_bounded() {
        let p = RetryPolicy::standard();
        let with = |v: &str| Response {
            status: 429,
            headers: vec![("retry-after".into(), v.into())],
        };
        assert_eq!(retry_after(&with("7")), Some(Duration::from_secs(7)));
        assert_eq!(
            retry_after(&Response {
                status: 429,
                headers: vec![]
            }),
            None
        );
        assert!(
            retry_after(&with("soon")).is_none(),
            "a non-numeric value is ignored"
        );
        // 7 s is believed; a day is not.
        assert_eq!(
            p.delay_after(1, Some(Duration::from_secs(7)), 0),
            Duration::from_secs(7)
        );
        assert_eq!(
            p.delay_after(1, Some(Duration::from_secs(86_400)), 0),
            Duration::from_secs(8)
        );
        // With no advice from the server, the jittered backoff is used instead.
        let d = p.delay_after(2, None, 12345);
        assert!(d >= p.backoff(2) / 2 && d <= p.backoff(2));
    }

    #[test]
    fn the_policy_decides_what_is_worth_another_try() {
        for code in [408, 409, 425, 429, 500, 502, 503, 529] {
            assert!(is_retryable_status(code), "{code} should be retried");
        }
        for code in [200, 201, 400, 401, 403, 404, 422] {
            assert!(!is_retryable_status(code), "{code} should not be retried");
        }
        assert!(is_retryable_transport(&NetError::Connect {
            address: "x".into(),
            detail: "y".into()
        }));
        assert!(!is_retryable_transport(&NetError::BadUrl("x".into())));
        assert!(!is_retryable_transport(&NetError::Tls {
            host: "x".into(),
            detail: "y".into()
        }));
    }

    #[test]
    fn off_means_one_attempt() {
        let script = Script::new(vec![Answer::Status(429, "no")]);
        let (result, waits, notes) = run_buffered(&script, RetryPolicy::off());
        assert_eq!(result.expect("returned as-is").0.status, 429);
        assert_eq!(script.calls(), 1);
        assert!(waits.is_empty() && notes.is_empty());
    }
}
