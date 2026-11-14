// The aina-usage D1 database (binding USAGE, migrations-usage/, docs/usage-analytics.md): every AI call and
// its daily totals, for the business reports (admin API) and the users' leaderboard. Best effort: without
// the binding nothing is kept and nothing fails. The rules live in badi_cloud_core::usage.

use badi_cloud_core::usage::{HELP_ROUTES, token_route};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, Env, Result};

fn n(v: i64) -> JsValue {
    JsValue::from_f64(v as f64)
}

fn s(v: &str) -> JsValue {
    JsValue::from_str(v)
}

/// One AI call as it is kept (ai_calls) and added to its day's totals (ai_usage_daily).
#[derive(Default)]
pub struct Row {
    pub at: i64,
    pub day: String,
    pub hour: i64,
    pub user: i64,
    pub provider: String,
    pub route: String,
    pub model: String,
    pub feature: String,
    pub version: String,
    pub outcome: String,
    pub status: i64,
    pub reason: String,
    pub stream: bool,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub audio_in: i64,
    pub audio_out: i64,
    pub searches: i64,
    pub audio_ms: i64,
    pub chars: i64,
    pub cost_milli_idr: i64,
    pub credits_milli: i64,
    pub first_ms: i64,
    pub total_ms: i64,
}

/// The sums every report reads, as SQL (over ai_usage_daily).
const SUMS: &str = "SUM(calls_ok) AS calls_ok, SUM(calls_failed) AS calls_failed, \
    SUM(input_tokens) AS input_tokens, SUM(output_tokens) AS output_tokens, \
    SUM(cache_read_tokens) AS cache_read_tokens, SUM(cache_write_tokens) AS cache_write_tokens, \
    SUM(audio_in_tokens) AS audio_in_tokens, SUM(audio_out_tokens) AS audio_out_tokens, \
    SUM(web_searches) AS web_searches, SUM(audio_ms) AS audio_ms, SUM(chars) AS chars, \
    SUM(cost_milli_idr) AS cost_milli_idr, SUM(credits_milli) AS credits_milli, SUM(total_ms) AS total_ms, \
    SUM(CASE WHEN route IN ('messages', 'realtime') THEN calls_ok ELSE 0 END) AS helps, \
    COUNT(DISTINCT CASE WHEN calls_ok > 0 THEN user_id END) AS users, \
    COUNT(DISTINCT CASE WHEN calls_ok > 0 THEN day END) AS active_days";

/// A report row: the group's keys as they came, then the sums with the numbers worked out from them.
fn shape(row: Map<String, Value>) -> Value {
    let f = |k: &str| row.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let tokens = f("input_tokens") + f("output_tokens") + f("cache_read_tokens") + f("cache_write_tokens") + f("audio_in_tokens") + f("audio_out_tokens");
    let mut out = Map::new();
    for (k, v) in &row {
        if !k.ends_with("_milli") && !k.ends_with("_milli_idr") && k != "total_ms" {
            out.insert(k.clone(), if v.is_null() { json!(0) } else { v.clone() });
        }
    }
    out.insert("tokens".into(), json!(tokens));
    out.insert("audioMinutes".into(), json!((f("audio_ms") / 600.0).round() / 100.0));
    if let Value::Object(d) = badi_cloud_core::usage::derived(f("cost_milli_idr"), f("credits_milli"), tokens, f("calls_ok"), f("calls_failed"), f("total_ms")) {
        out.extend(d);
    }
    Value::Object(out)
}

pub struct UsageDb(pub D1Database);

impl UsageDb {
    /// The database, when the Worker has it.
    pub fn of(env: &Env) -> Option<UsageDb> {
        env.d1("USAGE").ok().map(UsageDb)
    }

    async fn rows(&self, sql: &str, args: &[JsValue]) -> Result<Vec<Map<String, Value>>> {
        self.0.prepare(sql).bind(args)?.all().await?.results::<Map<String, Value>>()
    }

    /// Keeps a call and adds it to its day's totals, together (one batch).
    pub async fn record(&self, r: &Row) -> Result<()> {
        let ok = r.outcome == "ok";
        let call = self
            .0
            .prepare(
                "INSERT INTO ai_calls (at, day, hour, user_id, provider, route, model, feature, app_version, outcome, status, reason, stream, \
                 input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, audio_in_tokens, audio_out_tokens, web_searches, audio_ms, chars, \
                 cost_milli_idr, credits_milli, first_ms, total_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
            )
            .bind(&[
                n(r.at), s(&r.day), n(r.hour), n(r.user), s(&r.provider), s(&r.route), s(&r.model), s(&r.feature), s(&r.version), s(&r.outcome), n(r.status), s(&r.reason), n(r.stream as i64),
                n(r.input), n(r.output), n(r.cache_read), n(r.cache_write), n(r.audio_in), n(r.audio_out), n(r.searches), n(r.audio_ms), n(r.chars),
                n(r.cost_milli_idr), n(r.credits_milli), n(r.first_ms), n(r.total_ms),
            ])?;
        let day = self
            .0
            .prepare(
                "INSERT INTO ai_usage_daily (day, user_id, provider, model, route, feature, calls_ok, calls_failed, \
                 input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, audio_in_tokens, audio_out_tokens, web_searches, audio_ms, chars, \
                 cost_milli_idr, credits_milli, total_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20) \
                 ON CONFLICT (day, user_id, provider, model, route, feature) DO UPDATE SET \
                 calls_ok = calls_ok + excluded.calls_ok, calls_failed = calls_failed + excluded.calls_failed, \
                 input_tokens = input_tokens + excluded.input_tokens, output_tokens = output_tokens + excluded.output_tokens, \
                 cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens, cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens, \
                 audio_in_tokens = audio_in_tokens + excluded.audio_in_tokens, audio_out_tokens = audio_out_tokens + excluded.audio_out_tokens, \
                 web_searches = web_searches + excluded.web_searches, audio_ms = audio_ms + excluded.audio_ms, chars = chars + excluded.chars, \
                 cost_milli_idr = cost_milli_idr + excluded.cost_milli_idr, credits_milli = credits_milli + excluded.credits_milli, \
                 total_ms = total_ms + excluded.total_ms",
            )
            .bind(&[
                s(&r.day), n(r.user), s(&r.provider), s(&r.model), s(&r.route), s(&r.feature), n(ok as i64), n(!ok as i64),
                n(r.input), n(r.output), n(r.cache_read), n(r.cache_write), n(r.audio_in), n(r.audio_out), n(r.searches), n(r.audio_ms), n(r.chars),
                n(r.cost_milli_idr), n(r.credits_milli), n(if ok { r.total_ms } else { 0 }),
            ])?;
        self.0.batch(vec![call, day]).await.map(|_| ())
    }

    /// Forgets the single calls from before `day` (their daily totals stay).
    pub async fn prune(&self, day: &str) -> Result<()> {
        self.0.prepare("DELETE FROM ai_calls WHERE day < ?1").bind(&[s(day)])?.run().await.map(|_| ())
    }

    // ---------- reports (admin API) ----------

    /// Everything from `from` to `to` (WIB days, inclusive), in one row.
    pub async fn total(&self, from: &str, to: &str) -> Result<Value> {
        let rows = self.rows(&format!("SELECT {SUMS} FROM ai_usage_daily WHERE day BETWEEN ?1 AND ?2"), &[s(from), s(to)]).await?;
        Ok(rows.into_iter().next().map(shape).unwrap_or(Value::Null))
    }

    /// The same, grouped by `by` (columns of ai_usage_daily, from a fixed list), in the `order` asked:
    /// cost (the default), helps, tokens or day.
    pub async fn grouped(&self, from: &str, to: &str, by: &[&str], order: &str, limit: i64) -> Result<Vec<Value>> {
        let cols = by.iter().filter(|c| ["day", "user_id", "provider", "model", "route", "feature"].contains(c)).copied().collect::<Vec<_>>().join(", ");
        let order = match order {
            "day" => "day",
            "helps" => "helps DESC, cost_milli_idr DESC",
            "tokens" => "SUM(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens + audio_in_tokens + audio_out_tokens) DESC",
            _ => "cost_milli_idr DESC",
        };
        let sql = format!("SELECT {cols}, {SUMS} FROM ai_usage_daily WHERE day BETWEEN ?1 AND ?2 GROUP BY {cols} ORDER BY {order} LIMIT ?3");
        Ok(self.rows(&sql, &[s(from), s(to), n(limit)]).await?.into_iter().map(shape).collect())
    }

    /// One person's days, by provider and model.
    pub async fn of_user(&self, user: i64, from: &str, to: &str) -> Result<Vec<Value>> {
        let sql = format!("SELECT day, provider, model, route, {SUMS} FROM ai_usage_daily WHERE user_id = ?1 AND day BETWEEN ?2 AND ?3 GROUP BY day, provider, model, route ORDER BY day, cost_milli_idr DESC");
        Ok(self.rows(&sql, &[n(user), s(from), s(to)]).await?.into_iter().map(shape).collect())
    }

    /// Calls per WIB hour (over the calls still kept one by one).
    pub async fn hours(&self, from: &str, to: &str) -> Result<Vec<Map<String, Value>>> {
        self.rows("SELECT hour, COUNT(*) AS calls, COUNT(DISTINCT user_id) AS users FROM ai_calls WHERE outcome = 'ok' AND day BETWEEN ?1 AND ?2 GROUP BY hour ORDER BY hour", &[s(from), s(to)]).await
    }

    // ---------- the leaderboard ----------

    /// The top `limit` people on the leaderboard by how often aina helped them from `from` to `to`.
    pub async fn leaders(&self, from: &str, to: &str, limit: i64) -> Result<Vec<Map<String, Value>>> {
        let routes = HELP_ROUTES.map(|r| format!("'{r}'")).join(", ");
        let sql = format!(
            "SELECT l.user_id, l.name, SUM(d.calls_ok) AS helps, COUNT(DISTINCT d.day) AS days \
             FROM leaderboard l JOIN ai_usage_daily d ON d.user_id = l.user_id \
             WHERE d.day BETWEEN ?1 AND ?2 AND d.route IN ({routes}) AND d.calls_ok > 0 \
             GROUP BY l.user_id, l.name ORDER BY helps DESC, days DESC, l.joined_at LIMIT ?3"
        );
        self.rows(&sql, &[s(from), s(to), n(limit)]).await
    }

    /// One person's helps and active days from `from` to `to`, their name if they joined, and their
    /// place among those who joined (None when they haven't, or have no help yet).
    pub async fn standing(&self, user: i64, from: &str, to: &str) -> Result<Value> {
        #[derive(Deserialize)]
        struct Mine {
            helps: Option<f64>,
            days: Option<f64>,
        }
        #[derive(Deserialize)]
        struct Name {
            name: String,
        }
        #[derive(Deserialize)]
        struct Ahead {
            n: f64,
        }
        let routes = HELP_ROUTES.map(|r| format!("'{r}'")).join(", ");
        let mine = self
            .0
            .prepare(format!("SELECT SUM(calls_ok) AS helps, COUNT(DISTINCT CASE WHEN calls_ok > 0 THEN day END) AS days FROM ai_usage_daily WHERE user_id = ?1 AND day BETWEEN ?2 AND ?3 AND route IN ({routes})"))
            .bind(&[n(user), s(from), s(to)])?
            .first::<Mine>(None)
            .await?;
        let helps = mine.as_ref().and_then(|m| m.helps).unwrap_or(0.0) as i64;
        let days = mine.and_then(|m| m.days).unwrap_or(0.0) as i64;
        let name = self.0.prepare("SELECT name FROM leaderboard WHERE user_id = ?1").bind(&[n(user)])?.first::<Name>(None).await?.map(|r| r.name);
        let rank = if name.is_some() && helps > 0 {
            let ahead = self
                .0
                .prepare(format!(
                    "SELECT COUNT(*) AS n FROM (SELECT d.user_id FROM ai_usage_daily d JOIN leaderboard l ON l.user_id = d.user_id \
                     WHERE d.day BETWEEN ?1 AND ?2 AND d.route IN ({routes}) GROUP BY d.user_id HAVING SUM(d.calls_ok) > ?3)"
                ))
                .bind(&[s(from), s(to), n(helps)])?
                .first::<Ahead>(None)
                .await?
                .map(|a| a.n as i64)
                .unwrap_or(0);
            Some(ahead + 1)
        } else {
            None
        };
        Ok(json!({ "joined": name.is_some(), "name": name, "helps": helps, "days": days, "rank": rank }))
    }

    pub async fn join(&self, user: i64, name: &str, now: i64) -> Result<()> {
        self.0
            .prepare("INSERT INTO leaderboard (user_id, name, joined_at) VALUES (?1, ?2, ?3) ON CONFLICT (user_id) DO UPDATE SET name = excluded.name")
            .bind(&[n(user), s(name), n(now)])?
            .run()
            .await
            .map(|_| ())
    }

    pub async fn leave(&self, user: i64) -> Result<()> {
        self.0.prepare("DELETE FROM leaderboard WHERE user_id = ?1").bind(&[n(user)])?.run().await.map(|_| ())
    }
}

/// Which of a call's five usage numbers are tokens: for the token routes all of them; for the others the
/// first holds characters or bytes (kept for the Analytics Engine trace), so none.
pub fn token_usage(route: &str, usage: [u64; 5]) -> [u64; 5] {
    if token_route(route) { usage } else { [0; 5] }
}
