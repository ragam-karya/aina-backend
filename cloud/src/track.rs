// Tracing of the AI calls, in two places:
// - Workers Analytics Engine (dataset aina_ai_calls, binding AI_CALLS): one data point per call, for quick
//   queries over the last three months (latency, errors). Queries: docs/analytics.md.
// - The aina-usage D1 database (binding USAGE, src/usage.rs): the same call kept for good in its daily
//   totals, for the business reports and the leaderboard. docs/usage-analytics.md.
// Who (account id), what (provider, route, model, the app's feature from x-aina-kind, app version), how it
// went (outcome, HTTP status), how much (tokens, characters, audio, the real cost in rupiah and the
// credits taken) and how fast. Never the prompt, screenshot, audio or answer. Best effort: a missing
// binding or a failed write never touches the call.

use badi_cloud_core::stats::{DAY_MS, wib_day};
use worker::*;

use crate::usage::{Row, UsageDb, token_usage};
use crate::{header, now};

/// What is known about a call when it starts.
pub struct Call {
    pub route: &'static str,
    /// Who aina pays: anthropic, openai, elevenlabs (badi_cloud_core::usage::provider).
    pub provider: &'static str,
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
            provider: badi_cloud_core::usage::provider(route, false),
            model: model.chars().take(64).collect(),
            kind: clip(header(req, "x-aina-kind")),
            version: clip(header(req, "x-aina-version")),
            user,
            start: now(),
        }
    }
}

/// What a call used besides text tokens.
#[derive(Default, Clone, Copy)]
pub struct Units {
    /// Realtime audio tokens in and out.
    pub audio_in: u64,
    pub audio_out: u64,
    /// Transcribed audio, ms.
    pub audio_ms: u64,
    /// Characters read out.
    pub chars: u64,
}

/// How a call ended.
pub struct Outcome<'a> {
    /// ok, refused (by aina Cloud's own rules), upstream (the provider answered with an error), error
    /// (the provider couldn't be reached).
    pub outcome: &'a str,
    pub status: u16,
    /// Refusal or error type, when there is one.
    pub reason: &'a str,
    /// input, output, cache read, cache write tokens, web searches. For the routes not billed in tokens
    /// the first is the characters or audio bytes sent (as the trace always had it).
    pub usage: [u64; 5],
    pub units: Units,
    /// What aina pays the provider, milli-rupiah (HPP; also for calls the person doesn't pay for).
    pub cost: i64,
    /// Credits taken from the account, in thousandths (MILLI).
    pub credits: i64,
    /// Time to the upstream's first byte (headers, or the first chunk of a stream), ms; 0 when unknown.
    pub first_ms: i64,
    pub stream: bool,
}

impl Outcome<'static> {
    pub fn ok(usage: [u64; 5], units: Units, cost: i64, credits: i64, first_ms: i64, stream: bool) -> Self {
        Outcome { outcome: "ok", status: 200, reason: "", usage, units, cost, credits, first_ms, stream }
    }

    /// The provider answered with an error status (nothing is booked).
    pub fn upstream(status: u16, first_ms: i64) -> Self {
        Outcome { outcome: "upstream", status, reason: "", usage: [0; 5], units: Units::default(), cost: 0, credits: 0, first_ms, stream: false }
    }

    /// The provider couldn't be reached.
    pub fn error() -> Self {
        Outcome { outcome: "error", status: 0, reason: "fetch", usage: [0; 5], units: Units::default(), cost: 0, credits: 0, first_ms: 0, stream: false }
    }
}

/// Writes the call to Analytics Engine and to the aina-usage database.
pub async fn record(env: &Env, c: &Call, o: Outcome<'_>) {
    let total_ms = now() - c.start;
    if let Ok(ds) = env.analytics_engine("AI_CALLS") {
        let user = c.user.to_string();
        let [i, out, r, w, s] = o.usage;
        let u = o.units;
        // double8: milli-rupiah of API cost (named "credits × 1000" before 2026-11-14; it always was rupiah).
        let _ = AnalyticsEngineDataPointBuilder::new()
            .indexes([user.as_str()])
            .blobs([c.route, c.model.as_str(), c.kind.as_str(), c.version.as_str(), o.outcome, o.reason, if o.stream { "stream" } else { "whole" }, user.as_str(), c.provider])
            .doubles([
                1.0, o.status as f64, i as f64, out as f64, r as f64, w as f64, s as f64, o.cost as f64, o.first_ms as f64, total_ms as f64,
                o.credits as f64, u.audio_in as f64, u.audio_out as f64, u.audio_ms as f64, u.chars as f64,
            ])
            .write_to(&ds);
    }
    let Some(db) = UsageDb::of(env) else { return };
    let [i, out, r, w, s] = token_usage(c.route, o.usage);
    let row = Row {
        at: c.start,
        day: wib_day(c.start),
        hour: (c.start + 7 * 3_600_000).rem_euclid(DAY_MS) / 3_600_000,
        user: c.user,
        provider: c.provider.into(),
        route: c.route.into(),
        model: c.model.clone(),
        feature: c.kind.clone(),
        version: c.version.clone(),
        outcome: o.outcome.into(),
        status: o.status as i64,
        reason: o.reason.into(),
        stream: o.stream,
        input: i as i64,
        output: out as i64,
        cache_read: r as i64,
        cache_write: w as i64,
        audio_in: o.units.audio_in as i64,
        audio_out: o.units.audio_out as i64,
        searches: s as i64,
        audio_ms: o.units.audio_ms as i64,
        chars: o.units.chars as i64,
        cost_milli_idr: o.cost,
        credits_milli: o.credits,
        first_ms: o.first_ms,
        total_ms,
    };
    if let Err(e) = db.record(&row).await {
        console_log!("[usage] record: {e}");
    }
}

/// A call aina Cloud turned away before it reached the provider.
pub async fn refused(env: &Env, c: &Call, status: u16, reason: &str) {
    record(env, c, Outcome { outcome: "refused", status, reason, usage: [0; 5], units: Units::default(), cost: 0, credits: 0, first_ms: 0, stream: false }).await;
}
