// Tracing of the AI calls: one data point per call in Workers Analytics Engine (dataset aina_ai_calls,
// binding AI_CALLS): who (account id), what (route, model, the app's feature from x-aina-kind, app
// version), how it went (outcome, HTTP status), how much (tokens, credits) and how fast (time to the
// upstream's first byte, total). Never the prompt, screenshot, audio or answer. Best effort: a missing
// binding or a failed write never touches the call. Queries: docs/analytics.md.

use worker::*;

use crate::{header, now};

/// What is known about a call when it starts.
pub struct Call {
    pub route: &'static str,
    pub model: String,
    pub kind: String,
    pub version: String,
    pub user: i64,
    pub start: i64,
}

impl Call {
    pub fn new(req: &Request, route: &'static str, model: &str, user: i64) -> Call {
        let clip = |s: Option<String>| s.unwrap_or_default().chars().take(40).collect::<String>();
        Call {
            route,
            model: model.chars().take(64).collect(),
            kind: clip(header(req, "x-aina-kind")),
            version: clip(header(req, "x-aina-version")),
            user,
            start: now(),
        }
    }
}

/// How a call ended.
pub struct Outcome<'a> {
    /// ok, refused (by aina Cloud's own rules), upstream (the provider answered with an error), error
    /// (the provider couldn't be reached).
    pub outcome: &'a str,
    pub status: u16,
    /// Refusal or error type, when there is one.
    pub reason: &'a str,
    /// input, output, cache read, cache write tokens, web searches (or characters / bytes for audio).
    pub usage: [u64; 5],
    /// Credits booked, in thousandths (MILLI).
    pub cost: i64,
    /// Time to the upstream's first byte (headers, or the first chunk of a stream), ms; 0 when unknown.
    pub first_ms: i64,
    pub stream: bool,
}

impl Outcome<'static> {
    pub fn ok(usage: [u64; 5], cost: i64, first_ms: i64, stream: bool) -> Self {
        Outcome { outcome: "ok", status: 200, reason: "", usage, cost, first_ms, stream }
    }

    /// The provider answered with an error status (nothing is booked).
    pub fn upstream(status: u16, first_ms: i64) -> Self {
        Outcome { outcome: "upstream", status, reason: "", usage: [0; 5], cost: 0, first_ms, stream: false }
    }

    /// The provider couldn't be reached.
    pub fn error() -> Self {
        Outcome { outcome: "error", status: 0, reason: "fetch", usage: [0; 5], cost: 0, first_ms: 0, stream: false }
    }
}

pub fn write(env: &Env, c: &Call, o: Outcome) {
    let Ok(ds) = env.analytics_engine("AI_CALLS") else { return };
    let user = c.user.to_string();
    let [i, out, r, w, s] = o.usage;
    let _ = AnalyticsEngineDataPointBuilder::new()
        .indexes([user.as_str()])
        .blobs([c.route, c.model.as_str(), c.kind.as_str(), c.version.as_str(), o.outcome, o.reason, if o.stream { "stream" } else { "whole" }, user.as_str()])
        .doubles([1.0, o.status as f64, i as f64, out as f64, r as f64, w as f64, s as f64, o.cost as f64, o.first_ms as f64, (now() - c.start) as f64])
        .write_to(&ds);
}

/// A call aina Cloud turned away before it reached the provider.
pub fn refused(env: &Env, c: &Call, status: u16, reason: &str) {
    write(env, c, Outcome { outcome: "refused", status, reason, usage: [0; 5], cost: 0, first_ms: 0, stream: false });
}
