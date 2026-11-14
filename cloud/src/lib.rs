// aina Cloud on Cloudflare Workers. People sign in with Google and get the free starter credits once per
// PC; they buy monthly subscriptions or top-up packs (Midtrans), from the price list in the `plans`
// table. The Worker holds the Anthropic, OpenAI, ElevenLabs and Composio keys and proxies the app's
// calls per account, booking each call's real cost as credits (after a streamed reply ends, even when
// the app hangs up midway). Prompts, screenshots, audio and answers pass through and are not stored.
// The rules live in badi-cloud-core, shared with native tests; this file is the HTTP and D1 glue.

mod db;
mod track;
mod usage;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use badi_cloud_core::plans::{Kind, MILLI, Plan, stt_cost, tts_cost};
use badi_cloud_core::rules::{self, Account, ComposioRoute, Refusal, UsageMeter, admit, composio_route};
use badi_cloud_core::{self as core, Paid};
use futures_util::StreamExt;
use serde_json::{Value, json};
use worker::js_sys::Uint8Array;
use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::db::Db;
use crate::track::{Outcome, Units};
use crate::usage::UsageDb;

const TASK_GRACE_MS: i64 = 3_600_000;

fn now() -> i64 {
    Date::now().as_millis() as i64
}

fn secret(env: &Env, name: &str) -> String {
    // Trimmed: a secret piped in from a shell can carry a trailing newline, which breaks signatures and keys.
    env.secret(name).map(|s| s.to_string()).or_else(|_| env.var(name).map(|v| v.to_string())).unwrap_or_default().trim().to_string()
}

fn json_res(status: u16, v: &Value) -> Result<Response> {
    Ok(Response::from_json(v)?.with_status(status))
}

fn refuse(r: Refusal) -> Result<Response> {
    json_res(r.status, &r.body())
}

fn error(status: u16, kind: &str, message: &str) -> Result<Response> {
    json_res(status, &core::error_body(kind, message, json!({})))
}

fn header(req: &Request, k: &str) -> Option<String> {
    req.headers().get(k).ok().flatten()
}

/// Our own sites: this Worker (PUBLIC_URL, and ALT_PUBLIC_URLS, its other hostnames while it moves
/// domain) and the website.
fn own_sites(env: &Env) -> Vec<String> {
    let mut sites = vec![secret(env, "PUBLIC_URL"), frontend_url(env)];
    sites.extend(core::url_list(&secret(env, "ALT_PUBLIC_URLS")));
    sites
}

/// The account behind a call: its token in x-api-key, xi-api-key or Authorization: Bearer (the app), or
/// the session cookie (the browser; a POST with it must come from our own pages: the website's, through
/// its /api proxy, or this Worker's).
async fn user_of(req: &Request, env: &Env, db: &Db) -> Result<Option<(Account, String)>> {
    let header_token = header(req, "x-api-key")
        .or_else(|| header(req, "xi-api-key"))
        .or_else(|| header(req, "authorization").and_then(|v| v.strip_prefix("Bearer ").map(String::from)));
    let token = match header_token {
        Some(t) => Some(t),
        None => {
            let t = core::cookie(header(req, "cookie").as_deref(), core::SESSION_COOKIE);
            let sites = own_sites(env);
            let sites: Vec<&str> = sites.iter().map(String::as_str).collect();
            if t.is_some() && req.method() != Method::Get && !core::same_origin(header(req, "origin").as_deref(), &sites) {
                return Ok(None);
            }
            t
        }
    };
    let Some(token) = token.filter(|t| t.starts_with(core::TOKEN_PREFIX)) else { return Ok(None) };
    let hash = core::sha256(&token);
    Ok(db.user_by_token(&hash, now()).await?.map(|a| (a, hash)))
}

/// An upstream call with a body of bytes.
async fn upstream(method: Method, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>) -> Result<Response> {
    let h = Headers::new();
    for (k, v) in headers {
        h.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(h);
    if let Some(b) = body {
        init.with_body(Some(JsValue::from(Uint8Array::from(&b[..]))));
    }
    Fetch::Request(Request::new_with_init(url, &init)?).send().await
}

/// Passes an upstream reply on as it streams.
fn pass(mut res: Response, stream_type: &str) -> Result<Response> {
    let status = res.status_code();
    let ctype = res.headers().get("content-type")?.unwrap_or_else(|| stream_type.to_string());
    let h = Headers::new();
    h.set("content-type", &ctype)?;
    Ok(Response::from_stream(res.stream()?)?.with_status(status).with_headers(h))
}

// Sends the usage a streamed reply carried once the stream is done with (finished or cut off).
struct Booked {
    meter: Rc<RefCell<UsageMeter>>,
    done: Option<futures_channel::oneshot::Sender<UsageMeter>>,
}

impl Drop for Booked {
    fn drop(&mut self) {
        if let Some(tx) = self.done.take() {
            let _ = tx.send(self.meter.borrow().clone());
        }
    }
}

// ---------- accounts ----------

/// Where Google sends the person back: the website's /api proxy, so Google shows the website's address
/// (it must be an Authorized redirect URI of the OAuth client). The app's logins come back here too;
/// their state is in D1, so the address they started from doesn't matter.
fn callback_url(env: &Env) -> String {
    format!("{}/api/auth/google/callback", frontend_url(env))
}

/// The website with the sign-in, plans and thanks pages (FRONTEND_URL in wrangler.toml).
fn frontend_url(env: &Env) -> String {
    secret(env, "FRONTEND_URL").trim_end_matches('/').to_string()
}

fn page(html: String, status: u16) -> Result<Response> {
    Ok(Response::from_html(html)?.with_status(status))
}

/// A web login that didn't work: back to the website's sign-in page, which says why.
fn web_problem(env: &Env, message: &str, next: Option<&str>) -> Result<Response> {
    let next = next.map(|n| format!("&next={}", core::web::query_value(n))).unwrap_or_default();
    redirect_to(&format!("{}/masuk?error={}{next}", frontend_url(env), core::web::query_value(message)))
}

/// GET /login?app_state=… (the app) or /login?next=/langganan (the browser, through the website's
/// /api proxy, so the login nonce lands on the website's domain): off to Google.
async fn login(req: &Request, env: &Env, db: &Db) -> Result<Response> {
    let url = req.url()?;
    let q = |k: &str| url.query_pairs().find(|(name, _)| name == k).map(|(_, v)| v.to_string()).unwrap_or_default();
    let app_state = q("app_state");
    // A web login: back to a page of the website (any other target falls back to the plans).
    let next = (app_state.is_empty()).then(|| core::safe_next(&q("next")).unwrap_or_else(|| "/langganan".into()));
    if next.is_none() && !core::valid_app_state(&app_state) {
        return page(core::login_problem_page("Permintaan masuk tidak lengkap."), 400);
    }
    let client = secret(env, "GOOGLE_CLIENT_ID");
    if client.is_empty() {
        return match &next {
            Some(n) => web_problem(env, "Login Google belum disiapkan di server.", Some(n)),
            None => page(core::login_problem_page("Login Google belum disiapkan di server."), 503),
        };
    }
    let state = core::random_hex(24);
    let t = now();
    let nonce = next.is_some().then(|| core::random_hex(16));
    let kept = match &nonce {
        Some(n) => core::web_login_state(n),
        None => app_state,
    };
    db.put_oauth_state(&state, &kept, next.as_deref(), t + core::OAUTH_STATE_TTL_MS, t).await?;
    // Built by hand: Response::redirect's headers can't take the nonce cookie.
    let mut res = Response::empty()?.with_status(302);
    res.headers_mut().set("location", &core::google_auth_url(&client, &callback_url(env), &state))?;
    if let Some(n) = nonce {
        res.headers_mut().set("set-cookie", &core::login_cookie(&n))?;
    }
    Ok(res)
}

/// GET /auth/google/callback?code=…&state=…: the account, then back to the app with a one-use code, or
/// (a web login) on to the website's /api/auth/finish with one.
async fn google_callback(req: &Request, env: &Env, db: &Db) -> Result<Response> {
    let url = req.url()?;
    let q = |k: &str| url.query_pairs().find(|(name, _)| name == k).map(|(_, v)| v.to_string()).unwrap_or_default();
    let t = now();
    // Unknown (expired or used): said on the website, as nothing tells a web login from the app's.
    let Some((kept, next)) = db.take_oauth_state(&q("state"), t).await? else {
        return web_problem(env, "Waktu login sudah habis. Coba masuk lagi.", None);
    };
    let web = next.as_deref().and_then(core::safe_next);
    let problem = |message: &str, status: u16| match &web {
        Some(n) => web_problem(env, message, Some(n)),
        None => page(core::login_problem_page(message), status),
    };
    if !q("error").is_empty() {
        return problem("Login Google dibatalkan.", 400);
    }
    let client = secret(env, "GOOGLE_CLIENT_ID");
    let form = core::google_token_form(&client, &secret(env, "GOOGLE_CLIENT_SECRET"), &callback_url(env), &q("code"));
    let mut res = upstream(Method::Post, core::GOOGLE_TOKEN, &[("content-type", "application/x-www-form-urlencoded".into())], Some(form.into_bytes())).await?;
    let tokens: Value = res.json().await.unwrap_or(Value::Null);
    let Some((email, name)) = tokens["id_token"].as_str().and_then(|id| core::google_profile(id, &client)) else {
        return problem("Google tidak memberi email yang terverifikasi.", 400);
    };
    // The closed beta (BETA = "1"): only invited emails get in, and they get the beta credit.
    let beta = secret(env, "BETA") == "1";
    // UNLIMITED_EMAILS (comma-separated): accounts that use aina without limits, e.g. the owner's.
    let unlimited = secret(env, "UNLIMITED_EMAILS").split(',').any(|e| e.trim().eq_ignore_ascii_case(&email));
    if beta && !unlimited && !db.invited(&email.to_lowercase()).await? {
        return problem(&format!("aina masih beta tertutup, dan {email} belum ada di daftar undangan. Minta undangan ke tim aina, lalu masuk lagi."), 403);
    }
    // Composio links belong to this id; it says nothing about the person.
    let (account, _) = db.find_or_create(&email, name.as_deref(), t, &format!("aina_{}", core::random_hex(12)), beta, unlimited).await?;
    let code = core::random_hex(24);
    db.put_grant(&core::sha256(&code), account.id, &kept, t + core::GRANT_TTL_MS, t).await?;
    // The browser: on to the website, which trades the code for its session cookie. The free starter
    // is not given there; it comes with the first sign-in from the app on a PC.
    if let Some(next) = web {
        return redirect_to(&format!("{}/api/auth/finish?code={code}&next={}", frontend_url(env), core::web::query_value(&next)));
    }
    page(core::back_to_app_page(&code, &kept), 200)
}

/// GET /auth/finish?code=…&next=… (opened as FRONTEND_URL/api/auth/finish): a web login's one-use code,
/// from the browser that started it (its nonce in LOGIN_COOKIE), for the session cookie (a device token
/// labelled "web") on the website's domain, then on to `next`.
async fn auth_finish(req: &Request, env: &Env, db: &Db) -> Result<Response> {
    let url = req.url()?;
    let q = |k: &str| url.query_pairs().find(|(name, _)| name == k).map(|(_, v)| v.to_string()).unwrap_or_default();
    let next = core::safe_next(&q("next")).unwrap_or_else(|| "/langganan".into());
    let t = now();
    let nonce = core::cookie(header(req, "cookie").as_deref(), core::LOGIN_COOKIE).unwrap_or_default();
    let user = if nonce.is_empty() { None } else { db.take_grant(&core::sha256(&q("code")), &core::web_login_state(&nonce), t).await? };
    let mut res = match user {
        Some(user) => {
            let token = core::new_token();
            db.add_token(&core::sha256(&token), user, "web", t).await?;
            let mut res = redirect_to(&next)?;
            res.headers_mut().set("set-cookie", &core::session_cookie(&token))?;
            res
        }
        None => web_problem(env, "Link masuk ini sudah dipakai atau kedaluwarsa. Coba masuk lagi.", Some(&next))?,
    };
    res.headers_mut().append("set-cookie", &core::clear_login_cookie())?;
    Ok(res)
}

/// POST /v1/auth/exchange { code, app_state, device, hwid }: the app trades the one-use code for its
/// token. `hwid` (a hash of the PC's id) brings the free starter credits, once per PC and per account.
async fn exchange(mut req: Request, db: &Db) -> Result<Response> {
    let b: Value = req.json().await.unwrap_or(Value::Null);
    let t = now();
    let hash = core::sha256(b["code"].as_str().unwrap_or(""));
    let Some(user) = db.take_grant(&hash, b["app_state"].as_str().unwrap_or(""), t).await? else {
        return error(401, "authentication_error", "Link masuk ini sudah dipakai atau kedaluwarsa. Coba masuk lagi.");
    };
    let Some(account) = db.user(user).await? else { return refuse(rules::unauthorized()) };
    let token = core::new_token();
    let label: String = b["device"].as_str().unwrap_or("PC").chars().take(60).collect();
    db.add_token(&core::sha256(&token), user, &label, t).await?;
    let catalog = db.catalog().await?;
    // The free starter: given now if neither this account nor this PC has had it.
    let hwid = b["hwid"].as_str().unwrap_or("").to_lowercase();
    let mut free = 0;
    if rules::valid_hwid(&hwid)
        && let Some(credits) = rules::free_grant(&account, &catalog, db.pc_claimed(&hwid).await?)
        && db.claim_free(user, &hwid, credits, t).await?
    {
        free = credits / MILLI;
    }
    let account = db.user(user).await?.unwrap_or(account);
    json_res(200, &json!({ "token": token, "freeCredits": free, "account": account.view(&catalog, t) }))
}

// ---------- AI usage: the business reports (admin) and the leaderboard (docs/usage-analytics.md) ----------

/// GET /admin/usage[/summary|/daily|/features|/hours|/users|/user]: ?from=YYYY-MM-DD&to=YYYY-MM-DD (WIB
/// days, inclusive) or ?days=30, ?limit=, ?sort=cost|helps|tokens (users), ?email= (user).
async fn usage_report(env: &Env, db: &Db, what: &str, q: &std::collections::HashMap<String, String>, t: i64) -> Result<Response> {
    let Some(u) = UsageDb::of(env) else {
        return error(409, "invalid_request_error", "The USAGE database is not bound (wrangler.toml, [[d1_databases]] binding USAGE).");
    };
    let arg = |k: &str| q.get(k).map(String::as_str);
    let days = arg("days").and_then(|d| d.parse::<i64>().ok()).unwrap_or(30).clamp(1, 3660);
    let (from, to) = core::usage::range(arg("from"), arg("to"), days, t);
    let limit = arg("limit").and_then(|l| l.parse::<i64>().ok()).unwrap_or(50).clamp(1, 1000);
    let from_ms = core::usage::day_start_ms(&from).unwrap_or(0);
    let to_ms = core::usage::day_start_ms(&to).map(|d| d + core::stats::DAY_MS).unwrap_or(t);
    let cost_of = |v: &Value| v["costIdr"].as_f64().unwrap_or(0.0) * 1000.0;
    match what {
        "usage" | "usage/summary" => {
            let total = u.total(&from, &to).await?;
            let revenue: f64 = db.revenue(from_ms, to_ms).await?.iter().map(|(_, a)| a).sum();
            json_res(
                200,
                &json!({
                    "from": from, "to": to,
                    "total": total,
                    "margin": core::usage::margin(revenue, cost_of(&total)),
                    "byProvider": u.grouped(&from, &to, &["provider"], "cost", 100).await?,
                    "byModel": u.grouped(&from, &to, &["provider", "model", "route"], "cost", 200).await?,
                }),
            )
        }
        "usage/daily" => json_res(200, &json!({ "from": from, "to": to, "days": u.grouped(&from, &to, &["day"], "day", 3660).await? })),
        "usage/features" => json_res(200, &json!({ "from": from, "to": to, "features": u.grouped(&from, &to, &["feature"], "cost", 200).await? })),
        "usage/hours" => json_res(200, &json!({ "from": from, "to": to, "hours": u.hours(&from, &to).await? })),
        "usage/users" => {
            let mut rows = u.grouped(&from, &to, &["user_id"], arg("sort").unwrap_or("cost"), limit).await?;
            let ids: Vec<i64> = rows.iter().filter_map(|r| r["user_id"].as_f64()).map(|x| x as i64).collect();
            let people = db.people(&ids).await?;
            let paid = db.revenue(from_ms, to_ms).await?;
            for r in rows.iter_mut() {
                let id = r["user_id"].as_f64().unwrap_or(-1.0) as i64;
                let revenue = paid.iter().find(|(u, _)| *u == id).map(|(_, a)| *a).unwrap_or(0.0);
                let margin = core::usage::margin(revenue, cost_of(r));
                if let Some((_, email, name, plan)) = people.iter().find(|p| p.0 == id) {
                    r["email"] = json!(email);
                    r["name"] = json!(name);
                    r["plan"] = json!(plan);
                }
                r["margin"] = margin;
            }
            json_res(200, &json!({ "from": from, "to": to, "users": rows }))
        }
        "usage/user" => {
            let email = arg("email").and_then(core::clean_email).unwrap_or_default();
            let Some(id) = db.user_id_by_email(&email).await? else { return refuse(rules::not_found()) };
            let revenue: f64 = db.revenue(from_ms, to_ms).await?.iter().filter(|(u, _)| *u == id).map(|(_, a)| a).sum();
            let rows = u.of_user(id, &from, &to).await?;
            let cost: f64 = rows.iter().map(cost_of).sum();
            json_res(200, &json!({ "from": from, "to": to, "email": email, "userId": id, "margin": core::usage::margin(revenue, cost), "days": rows }))
        }
        _ => refuse(rules::not_found()),
    }
}

/// GET /v1/leaderboard?period=week|month|all: the top 20 of those who joined, by how often aina helped
/// them, and where the person stands. Only names people chose (or agreed to) are shown.
async fn leaderboard(req: &Request, env: &Env, a: &Account) -> Result<Response> {
    let Some(u) = UsageDb::of(env) else { return refuse(rules::not_found()) };
    let asked = req.url()?.query_pairs().find(|(k, _)| k == "period").map(|(_, v)| v.to_string()).unwrap_or_default();
    let (period, from, to) = core::usage::period(&asked, now());
    let top: Vec<Value> = u
        .leaders(&from, &to, 20)
        .await?
        .into_iter()
        .enumerate()
        .map(|(i, r)| json!({ "rank": i + 1, "name": r.get("name"), "helps": r.get("helps"), "days": r.get("days"), "me": r.get("user_id").and_then(Value::as_f64) == Some(a.id as f64) }))
        .collect();
    let me = u.standing(a.id, &from, &to).await?;
    json_res(200, &json!({ "period": period, "from": from, "to": to, "top": top, "me": me, "suggestedName": core::usage::default_name(a.name.as_deref()) }))
}

/// PUT /v1/leaderboard { name }: joins the leaderboard (or changes the name shown); without a name, the
/// first name and last initial Google gave.
async fn leaderboard_join(mut req: Request, env: &Env, a: &Account) -> Result<Response> {
    let Some(u) = UsageDb::of(env) else { return refuse(rules::not_found()) };
    let body: Value = req.json().await.unwrap_or(Value::Null);
    let name = match body["name"].as_str() {
        Some(raw) if !raw.trim().is_empty() => match core::usage::display_name(raw) {
            Some(n) => n,
            None => return error(400, "invalid_request_error", "Nama untuk leaderboard 2 sampai 24 huruf."),
        },
        _ => core::usage::default_name(a.name.as_deref()),
    };
    u.join(a.id, &name, now()).await?;
    let (period, from, to) = core::usage::period("week", now());
    json_res(200, &json!({ "period": period, "me": u.standing(a.id, &from, &to).await? }))
}

// ---------- admin: the price list, the beta invite list and credits (Postman: docs/aina-admin.postman_collection.json) ----------
// Every call needs `Authorization: Bearer <ADMIN_TOKEN>`; with ADMIN_TOKEN unset (or short) the API
// does not exist. Answers carry no prompts or usage details, only plans, invites and credits left.

async fn admin(mut req: Request, env: &Env, db: &Db, path: &str) -> Result<Response> {
    let token = secret(env, "ADMIN_TOKEN");
    let auth = header(&req, "authorization");
    // The usage reports only read: they also open with the simpler USAGE_BASIC_AUTH (wrangler.toml).
    let usage_read = req.method() == Method::Get && (path == "usage" || path.starts_with("usage/"));
    let ok = core::admin_ok(auth.as_deref(), &token) || (usage_read && core::basic_ok(auth.as_deref(), &secret(env, "USAGE_BASIC_AUTH")));
    if token.len() < 24 && !ok {
        return refuse(rules::not_found()); // no ADMIN_TOKEN: no admin API
    }
    if !ok {
        // Asks for the user name and password (Basic auth: admin + ADMIN_TOKEN); a Bearer token works too.
        let mut res = error(401, "authentication_error", "Masuk dulu (Basic auth).")?;
        res.headers_mut().set("www-authenticate", "Basic realm=\"aina admin\", charset=\"UTF-8\"")?;
        return Ok(res);
    }
    let t = now();
    let method = req.method();
    let query = |k: &str| req.url().ok().and_then(|u| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.to_string()));
    match (method, path) {
        // AI usage for the business (docs/usage-analytics.md), from the aina-usage database.
        (Method::Get, p) if p == "usage" || p.starts_with("usage/") => {
            let q: std::collections::HashMap<String, String> = req.url()?.query_pairs().into_owned().collect();
            usage_report(env, db, p, &q, t).await
        }
        // The kept daily numbers (docs/analytics.md): ?days=30 (at most 400).
        (Method::Get, "stats") => {
            let days = query("days").and_then(|d| d.parse::<i64>().ok()).unwrap_or(30).clamp(1, 400);
            let from = core::stats::wib_day(core::stats::wib_day_start(t) - days * core::stats::DAY_MS);
            json_res(200, &json!({ "from": from, "days": db.stats_since(&from).await? }))
        }
        // Runs the rollup now; ?redo=1 does the past week again (the days Analytics Engine still has).
        (Method::Post, "stats/roll") => {
            if secret(env, "CF_ANALYTICS_TOKEN").is_empty() {
                return error(409, "invalid_request_error", "CF_ANALYTICS_TOKEN is not set (wrangler secret put CF_ANALYTICS_TOKEN).");
            }
            if query("redo").as_deref() == Some("1") {
                let oldest = core::stats::days_to_check(t).last().map(|(d, _)| d.clone()).unwrap_or_default();
                db.unroll_since(&oldest).await?;
            }
            match roll_up(env, db, t).await {
                Ok(days) => json_res(200, &json!({ "rolled": days })),
                Err(e) => error(502, "api_error", &e.to_string()),
            }
        }
        (Method::Get, "beta") => {
            let credits = db.catalog().await?.of_kind(Kind::Beta).map(|p| p.credits).unwrap_or(0);
            json_res(200, &json!({ "beta": secret(env, "BETA") == "1", "credits": credits, "invites": db.invites().await? }))
        }
        (Method::Post, "beta") => {
            let b: Value = req.json().await.unwrap_or(Value::Null);
            let note = b["note"].as_str().unwrap_or("").chars().take(200).collect::<String>();
            let given: Vec<String> = match (&b["emails"], &b["email"]) {
                (Value::Array(list), _) => list.iter().filter_map(Value::as_str).map(String::from).collect(),
                (_, Value::String(one)) => one.split([',', '\n', ';']).map(String::from).collect(),
                _ => vec![],
            };
            let (mut added, mut already, mut invalid) = (vec![], vec![], vec![]);
            for raw in given.iter().filter(|e| !e.trim().is_empty()) {
                match core::clean_email(raw) {
                    Some(e) if db.invite(&e, &note, t).await? => added.push(e),
                    Some(e) => already.push(e),
                    None => invalid.push(raw.trim().to_string()),
                }
            }
            json_res(if added.is_empty() && !invalid.is_empty() { 400 } else { 200 }, &json!({ "added": added, "already": already, "invalid": invalid }))
        }
        (Method::Delete, "beta") => {
            let Some(email) = query("email").as_deref().and_then(core::clean_email) else { return error(400, "invalid_request_error", "?email= is needed.") };
            json_res(200, &json!({ "email": email, "removed": db.uninvite(&email).await? }))
        }
        // Credits that never lapse for one account: { "email": "…", "credits": 50 }.
        (Method::Post, "credit") => {
            let b: Value = req.json().await.unwrap_or(Value::Null);
            let Some(email) = b["email"].as_str().and_then(core::clean_email) else { return error(400, "invalid_request_error", "email is needed.") };
            let credits = b["credits"].as_f64().unwrap_or(0.0);
            if !(credits > 0.0 && credits <= 20_000.0) {
                return error(400, "invalid_request_error", "credits must be positive (at most 20,000).");
            }
            let Some(a) = db.user_by_email(&email).await? else { return error(404, "not_found", "No account with that email yet (they need to sign in once).") };
            db.add_topup(a.id, (credits * MILLI as f64).round() as i64).await?;
            let a = db.user(a.id).await?.ok_or("account gone")?;
            json_res(200, &json!({ "email": email, "account": a.view(&db.catalog().await?, t) }))
        }
        // Restricts an account: { "email": "…", "reason": "abuse" }. Every paid call is refused, and the
        // email stays restricted even if the account is deleted and signed in again.
        (Method::Post, "suspend") => {
            let b: Value = req.json().await.unwrap_or(Value::Null);
            let Some(email) = b["email"].as_str().and_then(core::clean_email) else { return error(400, "invalid_request_error", "email is needed.") };
            let reason: String = b["reason"].as_str().unwrap_or("admin").trim().chars().take(200).collect();
            let Some(a) = db.user_by_email(&email).await? else { return error(404, "not_found", "No account with that email.") };
            db.set_suspended(a.id, Some(if reason.is_empty() { "admin" } else { &reason }), t).await?;
            json_res(200, &json!({ "email": email, "suspended": true }))
        }
        // ?email=…: lifts the restriction.
        (Method::Delete, "suspend") => {
            let Some(email) = query("email").as_deref().and_then(core::clean_email) else { return error(400, "invalid_request_error", "?email= is needed.") };
            let Some(a) = db.user_by_email(&email).await? else { return error(404, "not_found", "No account with that email.") };
            db.set_suspended(a.id, None, t).await?;
            json_res(200, &json!({ "email": email, "suspended": false }))
        }
        // The price list, internal notes included, and what a credit is worth.
        (Method::Get, "plans") => {
            let c = db.catalog().await?;
            let plans: Vec<Value> = c.plans.iter().map(|p| { let mut v = p.public(); v["note"] = json!(p.note); v["sort"] = json!(p.sort); v["active"] = json!(p.active); v }).collect();
            json_res(200, &json!({ "plans": plans, "rupiahPerCredit": c.rupiah_per_credit, "enterpriseContact": c.enterprise_contact }))
        }
        // Adds or changes a plan: { "id": "pro", "price": 139000, "credits": 240 } (fields left out keep
        // their value; a new plan needs name and kind). "active": false stops offering it.
        (Method::Put, "plans") => {
            let b: Value = req.json().await.unwrap_or(Value::Null);
            let id = b["id"].as_str().unwrap_or("").trim().to_lowercase();
            if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
                return error(400, "invalid_request_error", "id is needed (letters, digits, _ or -).");
            }
            let c = db.catalog().await?;
            let base = c.get(&id).cloned();
            let kind = match b["kind"].as_str() {
                Some(k) => match Kind::parse(k) {
                    Some(k) => k,
                    None => return error(400, "invalid_request_error", "kind is one of free, subscription, topup, enterprise, beta."),
                },
                None => match &base {
                    Some(p) => p.kind,
                    None => return error(400, "invalid_request_error", "A new plan needs name and kind."),
                },
            };
            let mut p = base.unwrap_or_else(|| Plan::new(&id, &id, kind, 0, 0, 0, "", "", 100));
            p.kind = kind;
            let num = |k: &str, cur: i64| b[k].as_f64().map(|v| v.round() as i64).unwrap_or(cur);
            let text = |k: &str, cur: &str| b[k].as_str().map(|v| v.trim().chars().take(80).collect()).unwrap_or_else(|| cur.to_string());
            p.name = text("name", &p.name);
            p.badge = text("badge", &p.badge);
            p.note = text("note", &p.note);
            p.price = num("price", p.price);
            p.credits = num("credits", p.credits);
            p.days = num("days", p.days);
            p.sort = num("sort", p.sort);
            p.active = b["active"].as_bool().unwrap_or(p.active);
            if p.price < 0 || p.credits < 0 || p.days < 0 || p.name.is_empty() || (p.kind == Kind::Subscription && p.days == 0) {
                return error(400, "invalid_request_error", "price, credits and days can't be negative, the name can't be empty, and a subscription needs days.");
            }
            db.put_plan(&p, t).await?;
            let mut v = p.public();
            v["note"] = json!(p.note);
            v["active"] = json!(p.active);
            json_res(200, &v)
        }
        // Sign-in as aina (instead of Composio): which providers have aina's OAuth app, and the redirect
        // URI to give each provider's console.
        (Method::Get, "composio") => {
            let ac = auth_configs(env, db).await?;
            let providers: Vec<Value> = rules::COMPOSIO_PROVIDERS.iter().map(|(p, slugs)| json!({ "provider": p, "apps": slugs, "aina": slugs.iter().all(|s| ac[*s].is_string()) })).collect();
            json_res(200, &json!({ "redirectUris": { "composio": rules::COMPOSIO_CALLBACK, "viaAina": proxy_uri(env) }, "providers": providers, "authConfigs": ac }))
        }
        // { "provider": "google", "clientId": "…", "clientSecret": "…", "viaAina": false }: registers
        // aina's OAuth app at Composio for every app of the provider (all or none). viaAina: the provider returns to aina Cloud's redirect proxy (that URI
        // must be in the OAuth app) instead of
        // Composio's callback. Accounts linked before keep their old sign-in until they are relinked.
        (Method::Post, "composio") => {
            let b: Value = req.json().await.unwrap_or(Value::Null);
            let via = (b["viaAina"] == true).then(|| proxy_uri(env));
            let provider = b["provider"].as_str().unwrap_or("");
            let Some(slugs) = rules::provider_slugs(provider) else { return error(400, "invalid_request_error", "provider is one of google, microsoft, slack, github, notion, linear, jira, clickup, hubspot, salesforce, dropbox.") };
            let (id, secret_) = (b["clientId"].as_str().unwrap_or("").trim(), b["clientSecret"].as_str().unwrap_or("").trim());
            if id.is_empty() || secret_.is_empty() {
                return error(400, "invalid_request_error", "clientId and clientSecret are needed.");
            }
            let mut made: Vec<(&str, String)> = vec![];
            for slug in slugs {
                let (st, v) = composio_call(env, Method::Post, "auth_configs", "", Some(&rules::brand_auth_config(slug, id, secret_, via.as_deref()))).await?;
                match v["auth_config"]["id"].as_str() {
                    Some(ac) if st < 300 => made.push((slug, ac.to_string())),
                    _ => {
                        for (_, ac) in &made {
                            let _ = composio_call(env, Method::Delete, &format!("auth_configs/{ac}"), "", None).await;
                        }
                        return error(502, "api_error", &format!("Composio refused the OAuth app for {slug}: {}", v["error"]));
                    }
                }
            }
            let mut saved: Value = db.setting("composio_auth_configs").await?.and_then(|s| serde_json::from_str(&s).ok()).filter(Value::is_object).unwrap_or(json!({}));
            for (slug, ac) in &made {
                // The OAuth app registered before for this app is replaced and removed.
                if let Some(old) = saved[*slug].as_str().filter(|o| *o != ac) {
                    let _ = composio_call(env, Method::Delete, &format!("auth_configs/{old}"), "", None).await;
                }
                saved[*slug] = json!(ac);
            }
            db.put_setting("composio_auth_configs", &saved.to_string()).await?;
            json_res(200, &json!({ "provider": provider, "authConfigs": made.into_iter().map(|(s, a)| (s.to_string(), json!(a))).collect::<serde_json::Map<String, Value>>() }))
        }
        // ?provider=google: back to Composio's own sign-in for that provider's apps.
        (Method::Delete, "composio") => {
            let Some(slugs) = query("provider").as_deref().and_then(rules::provider_slugs) else { return error(400, "invalid_request_error", "?provider= is needed.") };
            let mut saved: Value = db.setting("composio_auth_configs").await?.and_then(|s| serde_json::from_str(&s).ok()).filter(Value::is_object).unwrap_or(json!({}));
            for slug in slugs {
                if let Some(old) = saved[*slug].as_str() {
                    let _ = composio_call(env, Method::Delete, &format!("auth_configs/{old}"), "", None).await;
                }
                if let Some(o) = saved.as_object_mut() {
                    o.remove(*slug);
                }
            }
            db.put_setting("composio_auth_configs", &saved.to_string()).await?;
            json_res(200, &json!({ "removed": slugs }))
        }
        // { "rupiahPerCredit": 170, "enterpriseContact": "mailto:sales@…" }
        (Method::Put, "settings") => {
            let b: Value = req.json().await.unwrap_or(Value::Null);
            if let Some(r) = b["rupiahPerCredit"].as_f64() {
                if !(r >= 1.0 && r <= 100_000.0) {
                    return error(400, "invalid_request_error", "rupiahPerCredit must be between 1 and 100,000.");
                }
                db.put_setting("rupiah_per_credit", &(r.round() as i64).to_string()).await?;
            }
            if let Some(contact) = b["enterpriseContact"].as_str() {
                let contact = contact.trim();
                if !(contact.is_empty() || contact.starts_with("mailto:") || contact.starts_with("https://")) {
                    return error(400, "invalid_request_error", "enterpriseContact is a mailto: or https: link (or empty).");
                }
                db.put_setting("enterprise_contact", contact).await?;
            }
            let c = db.catalog().await?;
            json_res(200, &json!({ "rupiahPerCredit": c.rupiah_per_credit, "enterpriseContact": c.enterprise_contact }))
        }
        _ => refuse(rules::not_found()),
    }
}

// ---------- billing ----------

/// POST /v1/checkout { item } from the app: the Midtrans payment page for it.
async fn checkout(mut req: Request, env: &Env, db: &Db, a: &Account) -> Result<Response> {
    let b: Value = req.json().await.unwrap_or(Value::Null);
    match start_checkout(env, db, a, b["item"].as_str().unwrap_or("")).await? {
        Ok((url, order)) => json_res(200, &json!({ "url": url, "order": order })),
        Err(r) => refuse(r),
    }
}

/// An order for an item the account may buy, and its Midtrans payment page: (url, order) or why not.
async fn start_checkout(env: &Env, db: &Db, a: &Account, item: &str) -> Result<std::result::Result<(String, String), Refusal>> {
    let t = now();
    let catalog = db.catalog().await?;
    let p = match rules::can_buy(a, &catalog, item, t) {
        Ok(p) => p,
        Err(r) => return Ok(Err(r)),
    };
    let key = secret(env, "MIDTRANS_SERVER_KEY");
    if key.is_empty() {
        return Ok(Err(rules::refusal(503, "api_error", "Pembayaran belum disiapkan di server.")));
    }
    let order = format!("aina-{}-{t}-{}", a.id, core::random_hex(3));
    db.add_payment(&order, a.id, &p.id, p.price, "checkout", t).await?;
    let finish = format!("{}/terima-kasih", frontend_url(env));
    let body = core::snap_order(&order, p, &a.email, &finish, &format!("aina-{}", a.id));
    let mut res = upstream(Method::Post, core::snap_url(midtrans_production(env)), &midtrans_headers(&key), Some(body.to_string().into_bytes())).await?;
    let v: Value = res.json().await.unwrap_or(Value::Null);
    Ok(match v["redirect_url"].as_str() {
        Some(url) => Ok((url.to_string(), order)),
        None => Err(rules::refusal(502, "api_error", &format!("Midtrans: {}", v["error_messages"]))),
    })
}

// ---------- subscribing from the browser ----------
// The pages are on the website (FRONTEND_URL), which calls this Worker through its /api proxy with the
// session cookie; here are only the account, the plans, checkout and the order, all JSON.

fn redirect_to(location: &str) -> Result<Response> {
    let mut res = Response::empty()?.with_status(303);
    res.headers_mut().set("location", location)?;
    Ok(res)
}

/// The pages that moved to the website: there, with the same path and query (/terima-kasih: an order
/// whose payment page was opened before the move still comes back here).
fn to_frontend(req: &Request, env: &Env) -> Result<Response> {
    let url = req.url()?;
    let query = url.query().map(|q| format!("?{q}")).unwrap_or_default();
    redirect_to(&format!("{}{}{query}", frontend_url(env), url.path()))
}

/// An account as /v1/account shows it, plus its cycle's end in words.
fn web_view(a: &Account, c: &badi_cloud_core::plans::Catalog) -> Value {
    let mut v = a.view(c, now());
    if v["subscribed"] == true {
        v["untilText"] = json!(format!("{} sampai {}", if v["renewal"]["auto"] == true { "diperpanjang" } else { "berakhir" }, badi_cloud_core::plans::date(a.plan_until)));
    }
    v
}

/// GET /v1/orders/{id}: the person's own order as the thanks page shows it, settled now when Midtrans
/// has it paid but its notification is late, with the account after it. Someone else's is not found.
async fn order(env: &Env, db: &Db, a: &Account, id: &str) -> Result<Response> {
    let Some(p) = db.payment(id).await?.filter(|p| p.user == a.id) else { return refuse(rules::not_found()) };
    let p = if p.status == "pending" {
        reconcile(env, db, Some(a.id)).await?;
        db.payment(id).await?.unwrap_or(p)
    } else {
        p
    };
    let c = db.catalog().await?;
    let a = db.user(a.id).await?.unwrap_or_else(|| a.clone());
    let plan = c.get(&p.item);
    let status = match p.status.as_str() {
        "paid" => "paid",
        "pending" => "pending",
        _ => "failed",
    };
    json_res(
        200,
        &json!({
            "order": id,
            "status": status,
            "item": p.item,
            "name": plan.map(|x| x.name.clone()).unwrap_or_else(|| p.item.clone()),
            "kind": plan.map(|x| x.kind.id()),
            "amount": p.amount,
            "credits": plan.map(|x| x.credits),
            "account": web_view(&a, &c),
        }),
    )
}

/// GET /download: the latest Windows installer from R2 (uploaded per release; see cloud/README.md).
async fn download(env: &Env) -> Result<Response> {
    let Ok(bucket) = env.bucket("DOWNLOADS") else { return page(core::web::download_soon_page(), 503) };
    let Some(obj) = bucket.get("windows/latest").execute().await? else { return page(core::web::download_soon_page(), 404) };
    let meta = obj.http_metadata();
    let Some(body) = obj.body() else { return page(core::web::download_soon_page(), 404) };
    let mut res = Response::from_stream(body.stream()?)?;
    let h = res.headers_mut();
    h.set("content-type", meta.content_type.as_deref().unwrap_or("application/octet-stream"))?;
    h.set("content-disposition", meta.content_disposition.as_deref().unwrap_or("attachment; filename=\"aina-setup.exe\""))?;
    h.set("content-length", &obj.size().to_string())?;
    h.set("cache-control", "no-store")?;
    Ok(res)
}

fn midtrans_production(env: &Env) -> bool {
    secret(env, "MIDTRANS_PRODUCTION") == "1"
}

fn midtrans_headers(key: &str) -> Vec<(&'static str, String)> {
    let auth = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, format!("{key}:"));
    vec![("authorization", format!("Basic {auth}")), ("content-type", "application/json".into()), ("accept", "application/json".into())]
}

async fn midtrans(mut req: Request, env: &Env, db: &Db) -> Result<Response> {
    let n: Value = req.json().await.unwrap_or(Value::Null);
    // Notifications are signed with the account's Signature Secret Key, which Midtrans now keeps apart
    // from the Server Key (API calls); older accounts sign with the Server Key, the fallback.
    let key = Some(secret(env, "MIDTRANS_SIGNATURE_KEY")).filter(|k| !k.is_empty()).unwrap_or_else(|| secret(env, "MIDTRANS_SERVER_KEY"));
    if !core::signature_ok(&n, &key) {
        // What was signed, and which key we checked it with (a short fingerprint, never the key), so a
        // mismatch can be traced: the fields can be re-signed by hand and the fingerprint compared.
        let s = |k: &str| n[k].as_str().map(String::from).unwrap_or_else(|| n[k].to_string());
        console_log!(
            "[billing] bad signature: order={} status_code={} gross_amount={} signature={}… key_sha256={}… key_len={}",
            s("order_id"), s("status_code"), s("gross_amount"), s("signature_key").chars().take(16).collect::<String>(),
            &core::sha256(&key)[..12], key.len()
        );
        return Response::error("Forbidden", 403);
    }
    settle(db, &n).await?;
    Response::ok("OK")
}

/// What Midtrans says about an order (a notification, a status lookup or a charge's answer, all the
/// same shape) applied once: a paid order delivers its plan; a card subscription keeps its card to renew.
async fn settle(db: &Db, n: &Value) -> Result<()> {
    let order = n["order_id"].as_str().unwrap_or("");
    match core::payment_outcome(n) {
        Paid::Yes => {
            let Some((user, item, amount, kind)) = db.mark_paid(order, now()).await? else { return Ok(()) };
            if amount != core::amount_of(n) {
                console_log!("[billing] {order}: paid {} but the order was {amount}", core::amount_of(n));
                return db.set_payment_status(order, "amount_mismatch").await;
            }
            // What the plan's row gives now (the price paid matched the order); a plan removed since is
            // logged for the team to settle by hand.
            let catalog = db.catalog().await?;
            match (catalog.get(&item), db.user(user).await?) {
                (Some(p), Some(_)) if p.kind == Kind::Topup => db.add_topup(user, p.credits * MILLI).await?,
                (Some(p), Some(a)) if p.kind == Kind::Subscription && kind == "renewal" => {
                    let (plan, until, sub_left) = rules::renew(&a, p, now());
                    db.start_subscription(user, &plan, until, sub_left).await?;
                }
                (Some(p), Some(a)) if p.kind == Kind::Subscription => {
                    let (plan, until, sub_left, _) = rules::purchase(&a, p, now());
                    db.start_subscription(user, &plan, until, sub_left).await?;
                    // Paid by card: it renews; any other way (QRIS, transfer, e-wallet): this cycle only.
                    match core::saved_card(n) {
                        Some((token, mask, expires)) => db.save_card(user, &token, &mask, expires).await?,
                        None => db.set_auto_renew(user, false).await?,
                    }
                }
                _ => console_log!("[billing] {order}: paid for {item}, which is no longer a plan to deliver"),
            }
        }
        Paid::Pending => {}
        Paid::Failed(status) => {
            // A payment already delivered and then refunded or charged back: its credits come back off
            // the account (into debt if spent), and a chargeback restricts it.
            if let Some(chargeback) = rules::reversal(&status)
                && let Some((user, item)) = db.mark_reversed(order, &status).await?
            {
                let t = now();
                if let (Some(p), Some(a)) = (db.catalog().await?.get(&item).cloned(), db.user(user).await?) {
                    let (sub, top, until) = rules::claw_back(&a, &p, t);
                    db.set_balances(user, sub, top, until).await?;
                    if p.kind == Kind::Subscription {
                        db.set_auto_renew(user, false).await?;
                    }
                }
                if chargeback {
                    db.drop_card(user).await?;
                    db.set_suspended(user, Some(&format!("chargeback {order}")), t).await?;
                }
                console_log!("[billing] {order}: {status} after delivery; credits taken back{}", if chargeback { ", account restricted" } else { "" });
                return Ok(());
            }
            db.set_payment_status(order, &status).await?
        }
    }
    Ok(())
}

/// Orders still pending (an account's, or all) looked up at Midtrans and settled: a lost or rejected
/// notification never leaves a paid order without its plan.
async fn reconcile(env: &Env, db: &Db, user: Option<i64>) -> Result<()> {
    let key = secret(env, "MIDTRANS_SERVER_KEY");
    if key.is_empty() {
        return Ok(());
    }
    let api = core::core_api(midtrans_production(env));
    for order in db.pending_payments(user, now() - RECONCILE_WINDOW_MS).await? {
        let Ok(mut res) = upstream(Method::Get, &format!("{api}/v2/{order}/status"), &midtrans_headers(&key), None).await else { continue };
        let v: Value = res.json().await.unwrap_or(Value::Null);
        // Not paid yet, or never opened: Midtrans has no transaction (404) and the order waits.
        if v["transaction_status"].is_string() && v["order_id"] == order.as_str() {
            settle(db, &v).await?;
        }
    }
    Ok(())
}

const RECONCILE_WINDOW_MS: i64 = 2 * 86_400_000;

/// Daily: settles what is pending, then charges the saved card of every card subscription due to renew
/// (from a day before its cycle ends). A refused charge is tried again the next day, up to RENEW_TRIES,
/// while the plan stays on in grace; after that renewing stops.
async fn renewals(env: &Env, db: &Db) -> Result<()> {
    reconcile(env, db, None).await?;
    let key = secret(env, "MIDTRANS_SERVER_KEY");
    if key.is_empty() {
        return Ok(());
    }
    let t = now();
    let catalog = db.catalog().await?;
    let api = core::core_api(midtrans_production(env));
    // Not one charged in the last 20 hours: a cycle is charged at most once a day.
    for a in db.renewal_candidates(t + badi_cloud_core::plans::DAY_MS, t - 20 * 3_600_000).await? {
        if !a.renewal_due(&catalog, t) {
            continue;
        }
        if a.card_lapsed(t) {
            console_log!("[billing] user {}: saved card lapses before the cycle ends; renewing stops", a.id);
            db.renewal_refused(a.id, rules::RENEW_TRIES, true).await?;
            continue;
        }
        let (Some(p), Some(token)) = (catalog.get(&a.plan), db.renewal_token(a.id).await?) else { continue };
        let order = format!("aina-{}-r{t}-{}", a.id, core::random_hex(3));
        db.add_payment(&order, a.id, &p.id, p.price, "renewal", t).await?;
        let body = core::renewal_charge(&order, p, &a.email, &token);
        let v: Value = match upstream(Method::Post, &format!("{api}/v2/charge"), &midtrans_headers(&key), Some(body.to_string().into_bytes())).await {
            Ok(mut res) => res.json().await.unwrap_or(Value::Null),
            Err(_) => Value::Null,
        };
        match core::payment_outcome(&v) {
            Paid::Yes => settle(db, &v).await?,
            // Midtrans is still deciding: the next reconcile settles it.
            Paid::Pending => {}
            Paid::Failed(status) => {
                db.set_payment_status(&order, if status.is_empty() { "refused" } else { &status }).await?;
                let (failures, stop) = rules::renewal_refused(&a);
                console_log!("[billing] user {}: renewal refused ({failures}/{}): {}", a.id, rules::RENEW_TRIES, v["status_message"]);
                db.renewal_refused(a.id, failures, stop).await?;
            }
        }
    }
    Ok(())
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let Ok(d1) = env.d1("DB") else { return };
    let db = Db(d1);
    if let Err(e) = renewals(&env, &db).await {
        console_log!("[billing] renewals: {e}");
    }
    match roll_up(&env, &db, now()).await {
        Ok(days) if !days.is_empty() => console_log!("[stats] rolled up {}", days.join(", ")),
        Ok(_) => {}
        Err(e) => console_log!("[stats] rollup: {e}"),
    }
    // Single AI calls are kept KEEP_CALLS_DAYS; their daily totals stay.
    if let Some(u) = UsageDb::of(&env) {
        let before = core::stats::wib_day(now() - core::usage::KEEP_CALLS_DAYS * core::stats::DAY_MS);
        if let Err(e) = u.prune(&before).await {
            console_log!("[usage] prune: {e}");
        }
    }
}

// ---------- the daily analytics rollup ----------

/// Asks Workers Analytics Engine's SQL API (the binding only writes). Needs CF_ACCOUNT_ID and the secret
/// CF_ANALYTICS_TOKEN (an API token with Account Analytics: Read).
async fn analytics_sql(env: &Env, sql: &str) -> Result<Value> {
    let url = format!("https://api.cloudflare.com/client/v4/accounts/{}/analytics_engine/sql", secret(env, "CF_ACCOUNT_ID"));
    let mut res = upstream(Method::Post, &url, &[("authorization", format!("Bearer {}", secret(env, "CF_ANALYTICS_TOKEN")))], Some(sql.as_bytes().to_vec())).await?;
    let status = res.status_code();
    let text = res.text().await?;
    if status >= 300 {
        return Err(Error::RustError(format!("analytics sql {status}: {}", text.chars().take(300).collect::<String>())));
    }
    serde_json::from_str(&text).map_err(|e| Error::RustError(format!("analytics sql: {e}")))
}

/// Keeps yesterday's numbers (and those of any day of the week before that isn't kept yet) in D1's
/// daily_stats, from Analytics Engine, which keeps only three months. Answers the days it kept. A day
/// whose queries fail stays undone and is tried on the next run.
async fn roll_up(env: &Env, db: &Db, t: i64) -> Result<Vec<String>> {
    if secret(env, "CF_ANALYTICS_TOKEN").is_empty() || secret(env, "CF_ACCOUNT_ID").is_empty() {
        return Ok(vec![]);
    }
    let days = core::stats::days_to_check(t);
    let done = db.rolled_days(&days.last().map(|(d, _)| d.clone()).unwrap_or_default()).await?;
    let mut kept = vec![];
    for (day, start) in days.into_iter().filter(|(d, _)| !done.contains(d)).take(core::stats::DAYS_PER_RUN) {
        let mut rows = vec![];
        for (metric, sql) in core::stats::queries(start) {
            let answer = analytics_sql(env, &sql).await.map_err(|e| Error::RustError(format!("{day} {metric}: {e}")))?;
            rows.extend(core::stats::rows(&answer).into_iter().map(|(dim, v)| (metric, dim, v)));
        }
        db.keep_day(&day, &rows, t).await?;
        kept.push(day);
    }
    Ok(kept)
}

// ---------- the proxy ----------

/// Checks that a call may start (not restricted, credits to start on, or a running task still inside
/// the overdraft); remembers a task started with credits so it may finish a little past zero (its
/// deadline is never stretched); and, with `slot`, takes one of the account's running-call slots. Ok:
/// the slot's id, for Db::end_call when the call is booked ("" without one).
async fn gate(req: &Request, db: &Db, a: &Account, slot: bool) -> Result<std::result::Result<String, Refusal>> {
    let t = now();
    let task = header(req, "x-aina-task").filter(|id| rules::valid_task(id));
    let continuing = match &task {
        Some(id) => db.task_continues(a.id, id, t).await?,
        None => false,
    };
    let catalog = db.catalog().await?;
    if let Err(r) = admit(a, &catalog, continuing, t) {
        return Ok(Err(r));
    }
    let mut call = String::new();
    if slot && !a.unlimited() {
        call = core::random_hex(12);
        if !db.begin_call(&call, a.id, t).await? {
            return Ok(Err(rules::refusal(429, "rate_limit_error", "Terlalu banyak permintaan aina berjalan bersamaan. Tunggu sebentar lalu coba lagi.")));
        }
    }
    if let Some(id) = task
        && a.left(&catalog, t) > 0
    {
        db.allow_task(a.id, &id, t + TASK_GRACE_MS, t).await?;
    }
    Ok(Ok(call))
}

/// Frees a call's slot (best effort: a slot left behind frees itself after INFLIGHT_MS).
async fn end_call(db: &Db, call: &str) {
    if !call.is_empty() {
        let _ = db.end_call(call).await;
    }
}

async fn messages(mut req: Request, env: Env, ctx: &Context, db: &Db, a: Account, count_only: bool) -> Result<Response> {
    let body = req.bytes().await?;
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let model = parsed["model"].as_str().unwrap_or("").to_string();
    let mut headers = vec![
        ("x-api-key", secret(&env, "ANTHROPIC_API_KEY")),
        ("anthropic-version", header(&req, "anthropic-version").unwrap_or_else(|| "2023-06-01".into())),
        ("content-type", "application/json".into()),
    ];
    if let Some(beta) = header(&req, "anthropic-beta") {
        headers.push(("anthropic-beta", beta));
    }
    if count_only {
        let res = upstream(Method::Post, "https://api.anthropic.com/v1/messages/count_tokens", &headers, Some(body)).await?;
        return pass(res, "application/json");
    }
    let traced = track::Call::new(&req, "messages", &model, a.id);
    if let Err(r) = rules::check_model(&model) {
        track::refused(&env, &traced, r.status, r.kind).await;
        return refuse(r);
    }
    let call = match gate(&req, db, &a, true).await? {
        Ok(c) => c,
        Err(r) => {
            track::refused(&env, &traced, r.status, r.kind).await;
            return refuse(r);
        }
    };
    let mut res = match upstream(Method::Post, "https://api.anthropic.com/v1/messages", &headers, Some(body)).await {
        Ok(r) => r,
        Err(e) => {
            end_call(db, &call).await;
            track::record(&env, &traced, Outcome::error()).await;
            return Err(e);
        }
    };
    if res.status_code() >= 300 {
        end_call(db, &call).await;
        track::record(&env, &traced, Outcome::upstream(res.status_code(), now() - traced.start)).await;
        return pass(res, "application/json");
    }
    let user = a.id;
    if parsed["stream"] != true {
        let bytes = res.bytes().await;
        let mut meter = UsageMeter::default();
        if let Ok(Ok(v)) = bytes.as_deref().map(serde_json::from_slice::<Value>) {
            meter.take(&v["usage"]);
        }
        // Whole replies: the first byte is the whole reply.
        let took = now() - traced.start;
        let cost = meter.cost(&model);
        let spent = db.spend(user, cost, now()).await;
        track::record(&env, &traced, Outcome::ok(meter.usage, Units::default(), cost, *spent.as_ref().unwrap_or(&0), took, false)).await;
        end_call(db, &call).await;
        spent?;
        let h = Headers::new();
        h.set("content-type", "application/json")?;
        return Ok(Response::from_bytes(bytes?)?.with_headers(h));
    }
    // Streamed: bill from the usage the stream carried, when it ends or the app hangs up.
    let (tx, rx) = futures_channel::oneshot::channel();
    let booked = Booked { meter: Rc::new(RefCell::new(UsageMeter::default())), done: Some(tx) };
    // When the first chunk of the reply came (time to first token, near enough).
    let first = Rc::new(Cell::new(0i64));
    let first_seen = first.clone();
    let stream = res.stream()?.map(move |chunk| {
        if let Ok(b) = &chunk {
            if first_seen.get() == 0 {
                first_seen.set(now());
            }
            booked.meter.borrow_mut().feed(b);
        }
        chunk
    });
    ctx.wait_until(async move {
        let meter = rx.await.ok();
        let db = env.d1("DB").ok().map(Db);
        if let Some(m) = &meter {
            let cost = m.cost(&model);
            let credits = match &db {
                Some(db) => db.spend(user, cost, now()).await.unwrap_or(0),
                None => 0,
            };
            let first_ms = if first.get() > 0 { first.get() - traced.start } else { 0 };
            track::record(&env, &traced, Outcome::ok(m.usage, Units::default(), cost, credits, first_ms, true)).await;
        }
        if let Some(db) = &db {
            end_call(db, &call).await;
        }
    });
    let h = Headers::new();
    h.set("content-type", "text/event-stream")?;
    h.set("cache-control", "no-cache")?;
    Ok(Response::from_stream(stream)?.with_headers(h))
}

/// A priced upstream call with a slot: booked at `cost` when the upstream takes it, then the slot freed.
/// `traced`, `usage` (characters or bytes sent) and `units` go to the call's trace.
#[allow(clippy::too_many_arguments)]
async fn paid_call(env: &Env, db: &Db, a: &Account, call: &str, cost: i64, res: Result<Response>, stream_type: &str, traced: &track::Call, usage: u64, units: Units) -> Result<Response> {
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            end_call(db, call).await;
            track::record(env, traced, Outcome::error()).await;
            return Err(e);
        }
    };
    let first_ms = now() - traced.start;
    let ok = res.status_code() < 300;
    let spent = if ok { db.spend(a.id, cost, now()).await } else { Ok(0) };
    let credits = *spent.as_ref().unwrap_or(&0);
    track::record(env, traced, if ok { Outcome::ok([usage, 0, 0, 0, 0], units, cost, credits, first_ms, false) } else { Outcome::upstream(res.status_code(), first_ms) }).await;
    end_call(db, call).await;
    spent?;
    pass(res, stream_type)
}

async fn transcribe(mut req: Request, env: &Env, db: &Db, a: Account) -> Result<Response> {
    let traced = track::Call::new(&req, "transcribe", "", a.id);
    let call = match gate(&req, db, &a, true).await? {
        Ok(c) => c,
        Err(r) => {
            track::refused(env, &traced, r.status, r.kind).await;
            return refuse(r);
        }
    };
    let ctype = header(&req, "content-type").unwrap_or_default();
    let body = match req.bytes().await {
        Ok(b) => b,
        Err(e) => {
            end_call(db, &call).await;
            return Err(e);
        }
    };
    let cost = stt_cost(body.len());
    let size = body.len() as u64;
    let units = Units { audio_ms: core::usage::pcm_ms(body.len()), ..Units::default() };
    let res = upstream(Method::Post, "https://api.openai.com/v1/audio/transcriptions", &[("authorization", format!("Bearer {}", secret(env, "OPENAI_API_KEY"))), ("content-type", ctype)], Some(body)).await;
    paid_call(env, db, &a, &call, cost, res, "application/json", &traced, size, units).await
}

async fn speech(mut req: Request, env: &Env, db: &Db, a: Account) -> Result<Response> {
    let body = req.bytes().await?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let traced = track::Call::new(&req, "speech", v["model"].as_str().unwrap_or(""), a.id);
    if let Err(r) = rules::check_tts_model(v["model"].as_str().unwrap_or("")) {
        track::refused(env, &traced, r.status, r.kind).await;
        return refuse(r);
    }
    let call = match gate(&req, db, &a, true).await? {
        Ok(c) => c,
        Err(r) => {
            track::refused(env, &traced, r.status, r.kind).await;
            return refuse(r);
        }
    };
    let chars = v["input"].as_str().unwrap_or("").chars().count();
    let cost = tts_cost(chars, false);
    let res = upstream(Method::Post, "https://api.openai.com/v1/audio/speech", &[("authorization", format!("Bearer {}", secret(env, "OPENAI_API_KEY"))), ("content-type", "application/json".into())], Some(body)).await;
    paid_call(env, db, &a, &call, cost, res, "application/octet-stream", &traced, chars as u64, Units { chars: chars as u64, ..Units::default() }).await
}

async fn eleven(mut req: Request, env: &Env, db: &Db, a: Account, voice: &str) -> Result<Response> {
    let query = req.url()?.query().map(|q| format!("?{q}")).unwrap_or_default();
    let body = req.bytes().await?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let traced = track::Call::new(&req, "eleven", v["model_id"].as_str().unwrap_or(""), a.id);
    let call = match gate(&req, db, &a, true).await? {
        Ok(c) => c,
        Err(r) => {
            track::refused(env, &traced, r.status, r.kind).await;
            return refuse(r);
        }
    };
    let chars = v["text"].as_str().unwrap_or("").chars().count();
    let cost = tts_cost(chars, true);
    let voice: String = voice.chars().filter(char::is_ascii_alphanumeric).collect();
    let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}/stream{query}");
    let res = upstream(Method::Post, &url, &[("xi-api-key", secret(env, "ELEVENLABS_API_KEY")), ("content-type", "application/json".into())], Some(body)).await;
    paid_call(env, db, &a, &call, cost, res, "application/octet-stream", &traced, chars as u64, Units { chars: chars as u64, ..Units::default() }).await
}

/// Percent-encodes a header value (a voice line can hold any letter of a nickname).
fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// POST /v1/voice/line { line, lang, name, wake, engine: "eleven" | "openai", voice, model }: one of aina's
/// fixed lines (the greeting, the replies when called, the filler, the out-of-credits line), spoken for
/// free, credits or not. The text is composed here from the line's id (badi_cloud_core::voice_lines), so
/// nothing else can be read out for free; it comes back percent-encoded in `x-aina-text` with the audio
/// (PCM, 24 kHz). Capped per account per WIB day: the app keeps every clip on disk.
async fn voice_line(mut req: Request, env: &Env, db: &Db, a: Account) -> Result<Response> {
    let v: Value = serde_json::from_slice(&req.bytes().await?).unwrap_or(Value::Null);
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let Some(text) = core::voice_lines::compose(&s("lang"), &s("line"), &s("name"), v["wake"] == true) else {
        return error(400, "invalid_request_error", "Unknown voice line.");
    };
    let eleven = s("engine") == "eleven";
    let mut traced = track::Call::new(&req, "voice_line", &s("model"), a.id);
    traced.provider = core::usage::provider("voice_line", eleven);
    let t = now();
    let refusal = if !eleven && let Err(r) = rules::check_tts_model(&s("model")) {
        Some(r)
    } else if !a.unlimited() && !db.take_voice_line(a.id, &core::stats::wib_day(t), core::voice_lines::DAILY_LIMIT).await? {
        Some(rules::refusal(429, "rate_limit_error", "Kalimat suara gratis aina untuk hari ini sudah habis. Coba lagi besok."))
    } else {
        None
    };
    if let Some(r) = refusal {
        track::refused(env, &traced, r.status, r.kind).await;
        return refuse(r);
    }
    let call = core::random_hex(12);
    if !a.unlimited() && !db.begin_call(&call, a.id, t).await? {
        let r = rules::refusal(429, "rate_limit_error", "Terlalu banyak permintaan aina berjalan bersamaan. Tunggu sebentar lalu coba lagi.");
        track::refused(env, &traced, r.status, r.kind).await;
        return refuse(r);
    }
    let lang = if s("lang") == "en" { "en" } else { "id" };
    let res = if eleven {
        let voice: String = s("voice").chars().filter(char::is_ascii_alphanumeric).collect();
        let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}/stream?output_format=pcm_24000");
        let body = json!({ "text": text, "model_id": s("model"), "language_code": lang });
        upstream(Method::Post, &url, &[("xi-api-key", secret(env, "ELEVENLABS_API_KEY")), ("content-type", "application/json".into())], Some(body.to_string().into_bytes())).await
    } else {
        let body = json!({ "model": s("model"), "voice": s("voice"), "input": text, "response_format": "pcm" });
        let auth = format!("Bearer {}", secret(env, "OPENAI_API_KEY"));
        upstream(Method::Post, "https://api.openai.com/v1/audio/speech", &[("authorization", auth), ("content-type", "application/json".into())], Some(body.to_string().into_bytes())).await
    };
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            end_call(db, &call).await;
            track::record(env, &traced, Outcome::error()).await;
            return Err(e);
        }
    };
    let first_ms = now() - traced.start;
    let ok = res.status_code() < 300;
    // Free for the person (no credits), but aina pays the provider: the trace keeps that cost.
    let chars = text.chars().count() as u64;
    let cost = tts_cost(chars as usize, eleven);
    track::record(env, &traced, if ok { Outcome::ok([chars, 0, 0, 0, 0], Units { chars, ..Units::default() }, cost, 0, first_ms, false) } else { Outcome::upstream(res.status_code(), first_ms) }).await;
    end_call(db, &call).await;
    let out = pass(res, "application/octet-stream")?;
    out.headers().set("x-aina-text", &pct(&text))?;
    Ok(out)
}

// Realtime voice: the app's WebRTC audio goes straight to OpenAI, so the cloud mints the one-minute
// client secret that opens the session (the account must have room left) and bills each reply from the
// usage the app reports from its response.done events.
async fn realtime_secret(mut req: Request, env: &Env, db: &Db, a: Account) -> Result<Response> {
    let traced = track::Call::new(&req, "realtime_session", "", a.id);
    if let Err(r) = gate(&req, db, &a, false).await? {
        track::refused(env, &traced, r.status, r.kind).await;
        return refuse(r);
    }
    let body: Value = req.json().await.unwrap_or(Value::Null);
    let out = match rules::realtime_secret_body(&body, a.plan == "beta") {
        Ok(b) => b,
        Err(r) => return refuse(r),
    };
    let mut res = upstream(Method::Post, "https://api.openai.com/v1/realtime/client_secrets", &[("authorization", format!("Bearer {}", secret(env, "OPENAI_API_KEY"))), ("content-type", "application/json".into())], Some(out.to_string().into_bytes())).await?;
    let status = res.status_code();
    let first_ms = now() - traced.start;
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if status >= 300 {
        track::record(env, &traced, Outcome::upstream(status, first_ms)).await;
        return json_res(status, &v);
    }
    track::record(env, &traced, Outcome::ok([0; 5], Units::default(), 0, 0, first_ms, false)).await;
    json_res(200, &json!({ "value": v["value"], "expires_at": v["expires_at"] }))
}

async fn realtime_usage(mut req: Request, env: &Env, db: &Db, a: Account) -> Result<Response> {
    let mut traced = track::Call::new(&req, "realtime", "", a.id);
    let v: Value = req.json().await.unwrap_or(Value::Null);
    let model = v["model"].as_str().unwrap_or("gpt-realtime");
    if !badi_cloud_core::plans::REALTIME_MODELS.contains(&model) {
        return refuse(rules::not_found());
    }
    let cost = badi_cloud_core::plans::realtime_cost(model, &v["usage"]);
    // The reply happened in the app's own session with OpenAI: no latency here, only what it used.
    traced.model = model.to_string();
    let t = core::usage::realtime_tokens(&v["usage"]);
    let spent = db.spend(a.id, cost, now()).await;
    let units = Units { audio_in: t.audio_in, audio_out: t.audio_out, ..Units::default() };
    track::record(env, &traced, Outcome::ok([t.text_in, t.text_out, t.cached, 0, 0], units, cost, *spent.as_ref().unwrap_or(&0), 0, false)).await;
    spent?;
    json_res(200, &json!({ "ok": true }))
}

async fn composio_call(env: &Env, method: Method, path: &str, query: &str, body: Option<&Value>) -> Result<(u16, Value)> {
    let url = format!("https://backend.composio.dev/api/v3.1/{path}{query}");
    let mut headers = vec![("x-api-key", secret(env, "COMPOSIO_API_KEY"))];
    if body.is_some() {
        headers.push(("content-type", "application/json".into()));
    }
    let mut res = upstream(method, &url, &headers, body.map(|b| b.to_string().into_bytes())).await?;
    let status = res.status_code();
    Ok((status, res.json().await.unwrap_or(Value::Null)))
}

/// aina Cloud's OAuth redirect proxy, for the providers' consoles.
/// Its base is COMPOSIO_REDIRECT_BASE when set, else PUBLIC_URL: the URI is kept by Composio and the
/// providers' consoles, so it must not move just because the Worker's address does.
fn proxy_uri(env: &Env) -> String {
    let base = Some(secret(env, "COMPOSIO_REDIRECT_BASE")).filter(|b| !b.is_empty()).unwrap_or_else(|| secret(env, "PUBLIC_URL"));
    format!("{}{}", base.trim_end_matches('/'), rules::REDIRECT_PROXY_PATH)
}

/// Sign-in as aina: { app slug: "ac_…" }, from the admin API (D1), over the older
/// COMPOSIO_AUTH_CONFIGS secret.
async fn auth_configs(env: &Env, db: &Db) -> Result<Value> {
    let mut all: Value = serde_json::from_str(&secret(env, "COMPOSIO_AUTH_CONFIGS")).unwrap_or(json!({}));
    if !all.is_object() {
        all = json!({});
    }
    if let Some(Value::Object(saved)) = db.setting("composio_auth_configs").await?.and_then(|s| serde_json::from_str(&s).ok()) {
        for (k, v) in saved {
            all[k.as_str()] = v;
        }
    }
    Ok(all)
}

async fn composio(mut req: Request, env: &Env, db: &Db, a: Account, path: &str) -> Result<Response> {
    // Work apps are for paying accounts; then, like every call, they need credits left.
    let paid = !a.unlimited() && db.has_paid(a.id).await?;
    if let Err(r) = rules::admit_apps(&a, &db.catalog().await?, paid, now()) {
        return refuse(r);
    }
    // Composio calls aren't billed, so they take no slot.
    if let Err(r) = gate(&req, db, &a, false).await? {
        return refuse(r);
    }
    let method = req.method();
    let query = req.url()?.query().map(|q| format!("?{q}")).unwrap_or_default();
    let bytes = req.bytes().await.unwrap_or_default();
    let body: Value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) };
    let auth_configs = auth_configs(env, db).await?;
    let (status, v) = match composio_route(method.as_ref(), path, &body, &a, &auth_configs) {
        ComposioRoute::NewSession(b) => {
            let (st, v) = composio_call(env, Method::Post, path, &query, Some(&b)).await?;
            if st < 300
                && let Some(id) = v["session_id"].as_str()
            {
                db.own_session(id, a.id).await?;
            }
            (st, v)
        }
        ComposioRoute::Session(id) => {
            if !db.owns_session(&id, a.id).await? {
                return refuse(rules::not_found());
            }
            composio_call(env, method, path, &query, (!body.is_null()).then_some(&body)).await?
        }
        ComposioRoute::LinkedAccount(id) => {
            let (_, acc) = composio_call(env, Method::Get, &format!("connected_accounts/{id}"), "", None).await?;
            if acc["user_id"].as_str() != Some(a.composio_user.as_str()) {
                return refuse(rules::not_found());
            }
            if method == Method::Get { (200, acc) } else { composio_call(env, method, path, "", None).await? }
        }
        ComposioRoute::Pass => composio_call(env, method, path, &query, (!body.is_null()).then_some(&body)).await?,
        ComposioRoute::Execute(b) => {
            // A run on a named linked account: only on the account's own (Composio would take another's).
            if let Some(id) = b["connected_account_id"].as_str() {
                let (_, acc) = composio_call(env, Method::Get, &format!("connected_accounts/{id}"), "", None).await?;
                if acc["user_id"].as_str() != Some(a.composio_user.as_str()) {
                    return refuse(rules::not_found());
                }
            }
            composio_call(env, Method::Post, path, &query, Some(&b)).await?
        }
        ComposioRoute::Deny => return refuse(rules::not_found()),
    };
    json_res(status, &v)
}

// ---------- routing ----------

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let path = req.path();
    let method = req.method();
    let db = Db(env.d1("DB")?);
    match (method.clone(), path.as_str()) {
        (Method::Get, "/") => return Response::ok("aina Cloud"),
        (Method::Get, "/privasi") => return Response::from_html(include_str!("privasi.html")),
        (Method::Get, "/ketentuan") => return Response::from_html(include_str!("ketentuan.html")),
        // The API docs (Swagger UI, docs/api/; rebuild with python docs/api/build.py). Endpoints only:
        // every admin call still needs ADMIN_TOKEN.
        (Method::Get, "/docs" | "/docs/") => return Response::from_html(include_str!("../../docs/api/index.html")),
        // The browser's pages live on the website now.
        (Method::Get, "/masuk" | "/langganan" | "/terima-kasih") => return to_frontend(&req, &env),
        (Method::Get, "/download" | "/download/windows") => return download(&env).await,
        (Method::Get, "/v1/plans") => return json_res(200, &db.catalog().await?.public()),
        // The provider's return after sign-in, sent on to Composio by the browser (a 302; the code and
        // token never pass through here).
        (Method::Get, rules::REDIRECT_PROXY_PATH) => {
            let query = req.url()?.query().unwrap_or("").to_string();
            return Response::redirect_with_status(Url::parse(&rules::composio_callback_url(&query))?, 302);
        }
        (Method::Get, "/login") => return login(&req, &env, &db).await,
        (Method::Get, "/auth/google/callback") => return google_callback(&req, &env, &db).await,
        (Method::Get, "/auth/finish") => return auth_finish(&req, &env, &db).await,
        (Method::Post, "/v1/auth/exchange") => return exchange(req, &db).await,
        (Method::Post, "/v1/billing/midtrans") => return midtrans(req, &env, &db).await,
        (_, p) if p.starts_with("/admin/") => {
            let rest = p.trim_start_matches("/admin/").to_string();
            return admin(req, &env, &db, &rest).await;
        }
        _ => {}
    }
    // Retiring an old address (docs/plan-custom-domain-be.md, BE-17): once RETIRED_HOST_MESSAGE is set,
    // the app's calls on an address in ALT_PUBLIC_URLS get that message instead of an answer. Installed
    // apps still built for that address show it (they print error.message), so it can point to the
    // update. Public pages, sign-in, downloads and the payment notifications above stay open.
    let notice = secret(&env, "RETIRED_HOST_MESSAGE");
    if !notice.is_empty() && core::host_listed(req.url()?.host_str().unwrap_or(""), &secret(&env, "ALT_PUBLIC_URLS")) {
        return error(426, "upgrade_required", &notice);
    }
    // Everything else needs an account.
    let Some((a, hash)) = user_of(&req, &env, &db).await? else { return refuse(rules::unauthorized()) };
    // A restricted account sees itself, stops renewing, signs out or deletes itself; nothing else.
    let allowed_restricted = matches!(
        (&method, path.as_str()),
        (Method::Get, "/v1/account") | (Method::Delete, "/v1/account") | (Method::Post, "/v1/auth/logout") | (Method::Post, "/v1/subscription/cancel") | (Method::Delete, "/v1/subscription/card")
    );
    if a.suspended.is_some() && !allowed_restricted {
        return refuse(rules::suspended());
    }
    match (method, path.as_str()) {
        (Method::Get, "/v1/account") => {
            // The app asks every minute (and every 15 s after a checkout): a payment whose notification
            // was lost is picked up here.
            if !db.pending_payments(Some(a.id), now() - RECONCILE_WINDOW_MS).await?.is_empty() {
                reconcile(&env, &db, Some(a.id)).await?;
            }
            let a = db.user(a.id).await?.unwrap_or(a);
            json_res(200, &web_view(&a, &db.catalog().await?))
        }
        // Card subscriptions: stop renewing (the plan runs to the end of its cycle), renew again, or
        // forget the card. Answers with the account as /v1/account does.
        (Method::Post, "/v1/subscription/cancel") => {
            db.set_auto_renew(a.id, false).await?;
            let a = db.user(a.id).await?.unwrap_or(a);
            json_res(200, &a.view(&db.catalog().await?, now()))
        }
        (Method::Post, "/v1/subscription/resume") => {
            let c = db.catalog().await?;
            if a.renewal.card.is_none() || a.subscription(&c, now()).is_none() {
                return error(409, "invalid_request_error", "Tidak ada langganan berbayar kartu yang bisa diperpanjang otomatis. Pilih paket lagi di Settings → Akun.");
            }
            db.set_auto_renew(a.id, true).await?;
            let a = db.user(a.id).await?.unwrap_or(a);
            json_res(200, &a.view(&c, now()))
        }
        (Method::Delete, "/v1/subscription/card") => {
            db.drop_card(a.id).await?;
            let a = db.user(a.id).await?.unwrap_or(a);
            json_res(200, &a.view(&db.catalog().await?, now()))
        }
        (Method::Delete, "/v1/account") => {
            // The usage stays, under the account number only (aina-usage holds no email); the name on the
            // leaderboard goes.
            if let Some(u) = UsageDb::of(&env) {
                let _ = u.leave(a.id).await;
            }
            db.delete_user(a.id).await?;
            json_res(200, &json!({ "deleted": true }))
        }
        (Method::Post, "/v1/auth/logout") => {
            db.drop_token(&hash).await?;
            // The browser's session cookie goes too (nothing for the app, which sends no cookie).
            let mut res = json_res(200, &json!({ "ok": true }))?;
            res.headers_mut().set("set-cookie", &core::clear_cookie())?;
            Ok(res)
        }
        (Method::Post, "/v1/checkout") => checkout(req, &env, &db, &a).await,
        (Method::Get, p) if p.starts_with("/v1/orders/") => order(&env, &db, &a, p.trim_start_matches("/v1/orders/")).await,
        (Method::Post, "/v1/messages") => messages(req, env.clone(), &ctx, &db, a, false).await,
        (Method::Post, "/v1/messages/count_tokens") => messages(req, env.clone(), &ctx, &db, a, true).await,
        (Method::Post, "/v1/audio/transcriptions") => transcribe(req, &env, &db, a).await,
        (Method::Post, "/v1/audio/speech") => speech(req, &env, &db, a).await,
        (Method::Post, "/v1/voice/line") => voice_line(req, &env, &db, a).await,
        (Method::Post, "/v1/realtime/client_secrets") => realtime_secret(req, &env, &db, a).await,
        (Method::Post, "/v1/realtime/usage") => realtime_usage(req, &env, &db, a).await,
        (Method::Get, "/v1/leaderboard") => leaderboard(&req, &env, &a).await,
        (Method::Put, "/v1/leaderboard") => leaderboard_join(req, &env, &a).await,
        (Method::Delete, "/v1/leaderboard") => {
            if let Some(u) = UsageDb::of(&env) {
                u.leave(a.id).await?;
            }
            json_res(200, &json!({ "joined": false }))
        }
        (Method::Post, p) if p.starts_with("/eleven/v1/text-to-speech/") && p.ends_with("/stream") => {
            let voice = p.trim_start_matches("/eleven/v1/text-to-speech/").trim_end_matches("/stream").to_string();
            eleven(req, &env, &db, a, &voice).await
        }
        (_, p) if p.starts_with("/composio/api/v3.1/") => {
            let rest = p.trim_start_matches("/composio/api/v3.1/").to_string();
            composio(req, &env, &db, a, &rest).await
        }
        _ => refuse(rules::not_found()),
    }
}
