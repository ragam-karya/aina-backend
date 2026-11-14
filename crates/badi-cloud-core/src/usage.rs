// AI usage for analytics and the business (docs/usage-analytics.md): every call aina Cloud makes to an AI
// provider, kept in its own D1 database (binding USAGE, migrations-usage/) so it can be read, moved or
// exported on its own. Per call: who, which provider and model, which of the app's features, how it went,
// the tokens (and characters or audio for voice), the real API cost in rupiah (HPP) and the credits taken
// from the account. Never the prompt, screenshot, audio or answer. This file holds the rules (providers,
// units, date windows, the numbers worked out from the totals, the leaderboard's names); the Worker does
// the D1.

use serde_json::{Value, json};

use crate::plans::{IDR_PER_USD, MILLI};
use crate::stats::{DAY_MS, wib_day, wib_day_start};

/// Calls are kept one by one for this long (ai_calls), then only in the daily totals (ai_usage_daily).
pub const KEEP_CALLS_DAYS: i64 = 180;

/// The routes whose calls are aina helping someone: an answer from Claude, or a spoken realtime reply.
/// (Transcribing a question and reading the answer out are parts of the same help.)
pub const HELP_ROUTES: [&str; 2] = ["messages", "realtime"];

/// Who aina pays for a call on `route` (`eleven`: a free voice line read by ElevenLabs).
pub fn provider(route: &str, eleven: bool) -> &'static str {
    match route {
        "messages" => "anthropic",
        "eleven" => "elevenlabs",
        "voice_line" if eleven => "elevenlabs",
        _ => "openai",
    }
}

/// Whether a route is billed in tokens (the others in characters or audio).
pub fn token_route(route: &str) -> bool {
    matches!(route, "messages" | "realtime")
}

/// The length of 16 kHz 16-bit mono audio, ms (what the app sends to transcribe).
pub fn pcm_ms(bytes: usize) -> u64 {
    bytes as u64 / 32
}

/// A realtime reply's tokens from its `response.done` usage: text (and images) in that wasn't cached,
/// cached tokens (text and audio), audio in that wasn't cached, text out, audio out.
#[derive(Debug, Default, PartialEq, Clone, Copy)]
pub struct RealtimeTokens {
    pub text_in: u64,
    pub cached: u64,
    pub audio_in: u64,
    pub text_out: u64,
    pub audio_out: u64,
}

pub fn realtime_tokens(u: &Value) -> RealtimeTokens {
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let inp = &u["input_token_details"];
    let hit = &inp["cached_tokens_details"];
    let (t_hit, a_hit) = (n(&hit["text_tokens"]), n(&hit["audio_tokens"]));
    RealtimeTokens {
        text_in: (n(&inp["text_tokens"]) + n(&inp["image_tokens"])).saturating_sub(t_hit),
        cached: t_hit + a_hit,
        audio_in: n(&inp["audio_tokens"]).saturating_sub(a_hit),
        text_out: n(&u["output_token_details"]["text_tokens"]),
        audio_out: n(&u["output_token_details"]["audio_tokens"]),
    }
}

/// "2026-11-14" → the UTC instant (ms) its WIB day starts; None for anything else.
pub fn day_start_ms(day: &str) -> Option<i64> {
    let mut it = day.split('-');
    let (y, m, d) = (it.next()?.parse::<i64>().ok()?, it.next()?.parse::<i64>().ok()?, it.next()?.parse::<i64>().ok()?);
    if it.next().is_some() || day.len() != 10 || !(2000..=2999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let start = days * DAY_MS - 7 * 3_600_000;
    // 2026-02-31 and the like don't come back as the same day.
    (wib_day(start) == day).then_some(start)
}

/// A report's WIB days, inclusive: `from` and `to` as given (YYYY-MM-DD), else the `days` days up to
/// today. A range the wrong way round is swapped.
pub fn range(from: Option<&str>, to: Option<&str>, days: i64, now: i64) -> (String, String) {
    let today = wib_day_start(now);
    let to = to.filter(|d| day_start_ms(d).is_some()).map(String::from).unwrap_or_else(|| wib_day(today));
    let from = from.filter(|d| day_start_ms(d).is_some()).map(String::from).unwrap_or_else(|| wib_day(today - (days.max(1) - 1) * DAY_MS));
    if from <= to { (from, to) } else { (to, from) }
}

/// The leaderboard's periods: this week (the last 7 WIB days), this month (the last 30) or all time.
pub fn period(name: &str, now: i64) -> (&'static str, String, String) {
    let today = wib_day_start(now);
    match name {
        "month" => ("month", wib_day(today - 29 * DAY_MS), wib_day(today)),
        "all" => ("all", "2000-01-01".into(), wib_day(today)),
        _ => ("week", wib_day(today - 6 * DAY_MS), wib_day(today)),
    }
}

/// A name for the leaderboard as typed: spaces tidied, no control characters, 2 to 24 characters.
pub fn display_name(raw: &str) -> Option<String> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ").chars().filter(|c| !c.is_control() && !matches!(c, '<' | '>')).take(24).collect::<String>();
    let name = name.trim().to_string();
    (name.chars().count() >= 2).then_some(name)
}

/// The leaderboard name before the person picks one: their first name and the initial of the last
/// ("Sari W."), from the name Google gave; "Pengguna aina" without one.
pub fn default_name(google: Option<&str>) -> String {
    let words: Vec<&str> = google.unwrap_or("").split_whitespace().collect();
    let name = match words.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [first, .., last] => format!("{first} {}.", last.chars().next().unwrap_or(' ')),
    };
    display_name(&name).unwrap_or_else(|| "Pengguna aina".into())
}

/// The numbers worked out from a group's totals (rupiah and credits in thousandths, as stored): the real
/// cost in rupiah and dollars, the credits, what a credit really cost, how many tokens a credit bought,
/// the failure rate and the mean time per call.
pub fn derived(cost_milli_idr: f64, credits_milli: f64, tokens: f64, calls_ok: f64, calls_failed: f64, total_ms: f64) -> Value {
    let cost_idr = cost_milli_idr / 1000.0;
    let credits = credits_milli / MILLI as f64;
    let calls = calls_ok + calls_failed;
    let r = |x: f64, p: i32| (x * 10f64.powi(p)).round() / 10f64.powi(p);
    json!({
        "costIdr": r(cost_idr, 2),
        "costUsd": r(cost_idr / IDR_PER_USD, 4),
        "credits": r(credits, 3),
        "idrPerCredit": if credits > 0.0 { json!(r(cost_idr / credits, 2)) } else { Value::Null },
        "tokensPerCredit": if credits > 0.0 && tokens > 0.0 { json!(r(tokens / credits, 0)) } else { Value::Null },
        "failRate": if calls > 0.0 { json!(r(calls_failed / calls, 4)) } else { Value::Null },
        "avgMs": if calls_ok > 0.0 { json!(r(total_ms / calls_ok, 0)) } else { Value::Null },
    })
}

/// Revenue against the real API cost: the gross margin, and whether the API cost more than came in.
pub fn margin(revenue_idr: f64, cost_milli_idr: f64) -> Value {
    let cost = cost_milli_idr / 1000.0;
    let profit = revenue_idr - cost;
    json!({
        "revenueIdr": revenue_idr,
        "costIdr": (cost * 100.0).round() / 100.0,
        "grossProfitIdr": (profit * 100.0).round() / 100.0,
        "grossMargin": if revenue_idr > 0.0 { json!(((profit / revenue_idr) * 10_000.0).round() / 10_000.0) } else { Value::Null },
        "loss": profit < 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-14 10:00 WIB.
    const NOW: i64 = 1_794_625_200_000;

    #[test]
    fn providers_follow_the_route() {
        assert_eq!(provider("messages", false), "anthropic");
        assert_eq!(provider("eleven", false), "elevenlabs");
        assert_eq!(provider("voice_line", true), "elevenlabs");
        assert_eq!(provider("voice_line", false), "openai");
        assert_eq!(provider("transcribe", false), "openai");
        assert_eq!(provider("realtime", false), "openai");
        assert!(token_route("messages") && token_route("realtime") && !token_route("speech"));
    }

    #[test]
    fn audio_length_from_pcm() {
        assert_eq!(pcm_ms(320_000), 10_000);
    }

    #[test]
    fn realtime_tokens_split_text_audio_and_cache() {
        let u = json!({ "input_token_details": { "text_tokens": 500, "image_tokens": 100, "audio_tokens": 1_000, "cached_tokens_details": { "text_tokens": 200, "audio_tokens": 400 } }, "output_token_details": { "text_tokens": 30, "audio_tokens": 600 } });
        assert_eq!(realtime_tokens(&u), RealtimeTokens { text_in: 400, cached: 600, audio_in: 600, text_out: 30, audio_out: 600 });
        assert_eq!(realtime_tokens(&json!({})), RealtimeTokens::default());
    }

    #[test]
    fn days_are_read_as_wib_days() {
        let start = day_start_ms("2026-11-14").unwrap();
        assert_eq!(wib_day(start), "2026-11-14");
        assert_eq!(wib_day(start - 1), "2026-11-13");
        assert_eq!(start, wib_day_start(NOW));
        for bad in ["2026-02-31", "2026-13-01", "26-11-14", "2026-11-14x", "", "2026-1-14"] {
            assert_eq!(day_start_ms(bad), None, "{bad}");
        }
        assert!(day_start_ms("2028-02-29").is_some());
    }

    #[test]
    fn ranges_default_to_the_last_days_and_take_valid_dates() {
        assert_eq!(range(None, None, 30, NOW), ("2026-10-16".into(), "2026-11-14".into()));
        assert_eq!(range(Some("2026-11-01"), Some("2026-11-07"), 30, NOW), ("2026-11-01".into(), "2026-11-07".into()));
        assert_eq!(range(Some("2026-11-07"), Some("2026-11-01"), 30, NOW), ("2026-11-01".into(), "2026-11-07".into()));
        assert_eq!(range(Some("kemarin"), None, 1, NOW), ("2026-11-14".into(), "2026-11-14".into()));
    }

    #[test]
    fn leaderboard_periods() {
        assert_eq!(period("week", NOW), ("week", "2026-11-08".into(), "2026-11-14".into()));
        assert_eq!(period("month", NOW), ("month", "2026-10-16".into(), "2026-11-14".into()));
        assert_eq!(period("all", NOW).1, "2000-01-01");
        assert_eq!(period("apa saja", NOW).0, "week");
    }

    #[test]
    fn leaderboard_names() {
        assert_eq!(display_name("  Sari   Wulandari \n").as_deref(), Some("Sari Wulandari"));
        assert_eq!(display_name("<b>x</b>").as_deref(), Some("bx/b"));
        assert_eq!(display_name(" a "), None);
        assert_eq!(display_name(&"x".repeat(40)).map(|n| n.len()), Some(24));
        assert_eq!(default_name(Some("Sari Wulandari Putri")), "Sari P.");
        assert_eq!(default_name(Some("Faisal")), "Faisal");
        assert_eq!(default_name(None), "Pengguna aina");
    }

    #[test]
    fn numbers_from_totals() {
        // 10 calls, Rp 1.700 of API cost booked as 10 credits, 50.000 tokens.
        let d = derived(1_700_000.0, 10_000.0, 50_000.0, 10.0, 0.0, 12_000.0);
        assert_eq!(d["costIdr"], 1700.0);
        assert_eq!(d["credits"], 10.0);
        assert_eq!(d["idrPerCredit"], 170.0);
        assert_eq!(d["tokensPerCredit"], 5000.0);
        assert_eq!(d["avgMs"], 1200.0);
        assert_eq!(d["failRate"], 0.0);
        // Nothing booked (an unlimited account, the free voice lines): no rate to show.
        assert!(derived(500_000.0, 0.0, 1_000.0, 1.0, 1.0, 0.0)["tokensPerCredit"].is_null());
    }

    #[test]
    fn margins() {
        let m = margin(49_000.0, 11_050_000.0);
        assert_eq!(m["grossProfitIdr"], 37_950.0);
        assert_eq!(m["grossMargin"], 0.7745);
        assert_eq!(m["loss"], false);
        assert_eq!(margin(0.0, 5_000.0)["loss"], true);
        assert!(margin(0.0, 0.0)["grossMargin"].is_null());
    }
}
