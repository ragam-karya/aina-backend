// Badi Cloud on Cloudflare Workers. People sign in with Google and get the free starter credits once per
// PC; they buy monthly subscriptions or top-up packs (Midtrans), from the price list in the `plans`
// table. The Worker holds the Anthropic, OpenAI, ElevenLabs and Composio keys and proxies the app's
// calls per account, booking each call's real cost as credits (after a streamed reply ends, even when
// the app hangs up midway). Prompts, screenshots, audio and answers pass through and are not stored.
// The rules live in badi-cloud-core, shared with native tests; this file is the HTTP and D1 glue.

mod db;

use std::cell::RefCell;
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
            if t.is_some() && req.method() != Method::Get && !core::same_origin(header(req, "origin").as_deref(), &[&secret(env, "PUBLIC_URL"), &frontend_url(env)]) {
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

fn callback_url(env: &Env) -> String {
    format!("{}/auth/google/callback", secret(env, "PUBLIC_URL").trim_end_matches('/'))
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
    // UNLIMITED_EMAILS (comma-separated): accounts that use Badi without limits, e.g. the owner's.
    let unlimited = secret(env, "UNLIMITED_EMAILS").split(',').any(|e| e.trim().eq_ignore_ascii_case(&email));
    if beta && !unlimited && !db.invited(&email.to_lowercase()).await? {
        return problem(&format!("aina masih beta tertutup, dan {email} belum ada di daftar undangan. Minta undangan ke tim aina, lalu masuk lagi."), 403);
    }
    // Composio links belong to this id; it says nothing about the person.
    let (account, _) = db.find_or_create(&email, name.as_deref(), t, &format!("badi_{}", core::random_hex(12)), beta, unlimited).await?;
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

// ---------- admin: the price list, the beta invite list and credits (Postman: docs/badi-admin.postman_collection.json) ----------
// Every call needs `Authorization: Bearer <ADMIN_TOKEN>`; with ADMIN_TOKEN unset (or short) the API
// does not exist. Answers carry no prompts or usage details, only plans, invites and credits left.

async fn admin(mut req: Request, env: &Env, db: &Db, path: &str) -> Result<Response> {
    if !core::admin_ok(header(&req, "authorization").as_deref(), &secret(env, "ADMIN_TOKEN")) {
        return refuse(rules::not_found());
    }
    let t = now();
    let method = req.method();
    let query = |k: &str| req.url().ok().and_then(|u| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.to_string()));
    match (method, path) {
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
        // Sign-in as B.A.D.I. (instead of Composio): which providers have B.A.D.I.'s OAuth app, and the
        // redirect URI to give each provider's console.
        (Method::Get, "composio") => {
            let ac = auth_configs(env, db).await?;
            let providers: Vec<Value> = rules::COMPOSIO_PROVIDERS.iter().map(|(p, slugs)| json!({ "provider": p, "apps": slugs, "badi": slugs.iter().all(|s| ac[*s].is_string()) })).collect();
            json_res(200, &json!({ "redirectUris": { "composio": rules::COMPOSIO_CALLBACK, "viaBadi": proxy_uri(env) }, "providers": providers, "authConfigs": ac }))
        }
        // { "provider": "google", "clientId": "…", "clientSecret": "…", "viaBadi": false }: registers
        // B.A.D.I.'s OAuth app at Composio for every app of the provider (all or none). viaBadi: the
        // provider returns to Badi Cloud's redirect proxy (that URI must be in the OAuth app) instead of
        // Composio's callback. Accounts linked before keep their old sign-in until they are relinked.
        (Method::Post, "composio") => {
            let b: Value = req.json().await.unwrap_or(Value::Null);
            let via = (b["viaBadi"] == true).then(|| proxy_uri(env));
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
    let order = format!("badi-{}-{t}-{}", a.id, core::random_hex(3));
    db.add_payment(&order, a.id, &p.id, p.price, "checkout", t).await?;
    let finish = format!("{}/terima-kasih", frontend_url(env));
    let body = core::snap_order(&order, p, &a.email, &finish, &format!("badi-{}", a.id));
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
    if !core::signature_ok(&n, &secret(env, "MIDTRANS_SERVER_KEY")) {
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
        Paid::Failed(status) => db.set_payment_status(order, &status).await?,
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
        let order = format!("badi-{}-r{t}-{}", a.id, core::random_hex(3));
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
    if let Err(e) = renewals(&env, &Db(d1)).await {
        console_log!("[billing] renewals: {e}");
    }
}

// ---------- the proxy ----------

/// Checks that a call has credits to start on; remembers its task so it may finish past zero.
async fn gate(req: &Request, db: &Db, a: &Account) -> Result<std::result::Result<(), Refusal>> {
    let t = now();
    let task = header(req, "x-badi-task");
    let continuing = match &task {
        Some(id) => db.task_continues(a.id, id, t).await?,
        None => false,
    };
    if let Err(r) = admit(a, &db.catalog().await?, continuing, t) {
        return Ok(Err(r));
    }
    if let Some(id) = task {
        db.allow_task(a.id, &id, t + TASK_GRACE_MS, t).await?;
    }
    Ok(Ok(()))
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
    if let Err(r) = gate(&req, db, &a).await? {
        return refuse(r);
    }
    if let Err(r) = rules::check_model(&model) {
        return refuse(r);
    }
    let mut res = upstream(Method::Post, "https://api.anthropic.com/v1/messages", &headers, Some(body)).await?;
    if res.status_code() >= 300 {
        return pass(res, "application/json");
    }
    let user = a.id;
    if parsed["stream"] != true {
        let bytes = res.bytes().await?;
        let mut meter = UsageMeter::default();
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            meter.take(&v["usage"]);
        }
        db.spend(user, meter.cost(&model), now()).await?;
        let h = Headers::new();
        h.set("content-type", "application/json")?;
        return Ok(Response::from_bytes(bytes)?.with_headers(h));
    }
    // Streamed: bill from the usage the stream carried, when it ends or the app hangs up.
    let (tx, rx) = futures_channel::oneshot::channel();
    let booked = Booked { meter: Rc::new(RefCell::new(UsageMeter::default())), done: Some(tx) };
    let stream = res.stream()?.map(move |chunk| {
        if let Ok(b) = &chunk {
            booked.meter.borrow_mut().feed(b);
        }
        chunk
    });
    ctx.wait_until(async move {
        if let Ok(m) = rx.await
            && let Ok(d1) = env.d1("DB")
        {
            let _ = Db(d1).spend(user, m.cost(&model), now()).await;
        }
    });
    let h = Headers::new();
    h.set("content-type", "text/event-stream")?;
    h.set("cache-control", "no-cache")?;
    Ok(Response::from_stream(stream)?.with_headers(h))
}

async fn transcribe(mut req: Request, env: &Env, db: &Db, a: Account) -> Result<Response> {
    if let Err(r) = gate(&req, db, &a).await? {
        return refuse(r);
    }
    let ctype = header(&req, "content-type").unwrap_or_default();
    let body = req.bytes().await?;
    let cost = stt_cost(body.len());
    let res = upstream(Method::Post, "https://api.openai.com/v1/audio/transcriptions", &[("authorization", format!("Bearer {}", secret(env, "OPENAI_API_KEY"))), ("content-type", ctype)], Some(body)).await?;
    if res.status_code() < 300 {
        db.spend(a.id, cost, now()).await?;
    }
    pass(res, "application/json")
}

async fn speech(mut req: Request, env: &Env, db: &Db, a: Account) -> Result<Response> {
    if let Err(r) = gate(&req, db, &a).await? {
        return refuse(r);
    }
    let body = req.bytes().await?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if let Err(r) = rules::check_tts_model(v["model"].as_str().unwrap_or("")) {
        return refuse(r);
    }
    let cost = tts_cost(v["input"].as_str().unwrap_or("").chars().count(), false);
    let res = upstream(Method::Post, "https://api.openai.com/v1/audio/speech", &[("authorization", format!("Bearer {}", secret(env, "OPENAI_API_KEY"))), ("content-type", "application/json".into())], Some(body)).await?;
    if res.status_code() < 300 {
        db.spend(a.id, cost, now()).await?;
    }
    pass(res, "application/octet-stream")
}

async fn eleven(mut req: Request, env: &Env, db: &Db, a: Account, voice: &str) -> Result<Response> {
    if let Err(r) = gate(&req, db, &a).await? {
        return refuse(r);
    }
    let query = req.url()?.query().map(|q| format!("?{q}")).unwrap_or_default();
    let body = req.bytes().await?;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let cost = tts_cost(v["text"].as_str().unwrap_or("").chars().count(), true);
    let voice: String = voice.chars().filter(char::is_ascii_alphanumeric).collect();
    let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}/stream{query}");
    let res = upstream(Method::Post, &url, &[("xi-api-key", secret(env, "ELEVENLABS_API_KEY")), ("content-type", "application/json".into())], Some(body)).await?;
    if res.status_code() < 300 {
        db.spend(a.id, cost, now()).await?;
    }
    pass(res, "application/octet-stream")
}

// Realtime voice: the app's WebRTC audio goes straight to OpenAI, so the cloud mints the one-minute
// client secret that opens the session (the account must have room left) and bills each reply from the
// usage the app reports from its response.done events.
async fn realtime_secret(mut req: Request, env: &Env, db: &Db, a: Account) -> Result<Response> {
    if let Err(r) = gate(&req, db, &a).await? {
        return refuse(r);
    }
    let body: Value = req.json().await.unwrap_or(Value::Null);
    let out = match rules::realtime_secret_body(&body, a.plan == "beta") {
        Ok(b) => b,
        Err(r) => return refuse(r),
    };
    let mut res = upstream(Method::Post, "https://api.openai.com/v1/realtime/client_secrets", &[("authorization", format!("Bearer {}", secret(env, "OPENAI_API_KEY"))), ("content-type", "application/json".into())], Some(out.to_string().into_bytes())).await?;
    let status = res.status_code();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if status >= 300 {
        return json_res(status, &v);
    }
    json_res(200, &json!({ "value": v["value"], "expires_at": v["expires_at"] }))
}

async fn realtime_usage(mut req: Request, db: &Db, a: Account) -> Result<Response> {
    let v: Value = req.json().await.unwrap_or(Value::Null);
    let model = v["model"].as_str().unwrap_or("gpt-realtime");
    if !badi_cloud_core::plans::REALTIME_MODELS.contains(&model) {
        return refuse(rules::not_found());
    }
    db.spend(a.id, badi_cloud_core::plans::realtime_cost(model, &v["usage"]), now()).await?;
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

/// Badi Cloud's OAuth redirect proxy, for the providers' consoles.
fn proxy_uri(env: &Env) -> String {
    format!("{}{}", secret(env, "PUBLIC_URL").trim_end_matches('/'), rules::REDIRECT_PROXY_PATH)
}

/// Sign-in as B.A.D.I.: { app slug: "ac_…" }, from the admin API (D1), over the older
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
    if let Err(r) = gate(&req, db, &a).await? {
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
        ComposioRoute::Execute(b) => composio_call(env, Method::Post, path, &query, Some(&b)).await?,
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
    // Everything else needs an account.
    let Some((a, hash)) = user_of(&req, &env, &db).await? else { return refuse(rules::unauthorized()) };
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
        (Method::Post, "/v1/realtime/client_secrets") => realtime_secret(req, &env, &db, a).await,
        (Method::Post, "/v1/realtime/usage") => realtime_usage(req, &db, a).await,
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
