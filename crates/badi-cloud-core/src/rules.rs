// What a call may do: whether an account may start it (credits left, a running task finishing), what
// may be bought and what a payment gives (no early renewal of a subscription; top-ups any time), the
// free starter once per PC, what a streamed Claude reply cost from the usage it carried, and how a
// Composio call is scoped to the account that makes it.

use serde_json::{Value, json};

use crate::plans::{Catalog, Kind, MILLI, Meter, Plan, claude_cost, date, model_allowed, search_cost, whole};

/// The account behind a call.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: i64,
    pub email: String,
    /// The name Google gives (None until the first sign-in that brings one).
    pub name: Option<String>,
    /// The subscription plan's id while there is or was one; "free" or "beta" otherwise.
    pub plan: String,
    /// When the current subscription cycle ends (0 without one).
    pub plan_until: i64,
    pub composio_user: String,
    pub meter: Meter,
    /// The free starter was given to this account.
    pub free_claimed: bool,
    pub renewal: Renewal,
    /// Why the account is restricted (a chargeback, an admin's call), or None. A restricted account can
    /// only see itself, sign out and delete itself; every paid call is refused.
    pub suspended: Option<String>,
}

/// A subscription paid by credit card renews itself with the card Midtrans saved (only its token is
/// kept, never the card), until the person cancels or the card is refused RENEW_TRIES times.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Renewal {
    /// Renews at the end of the cycle (off: cancelled, stopped, or never paid by card).
    pub auto: bool,
    /// A saved card to charge: "481111-1114" as Midtrans masks it.
    pub card: Option<String>,
    /// When the saved card's token stops working (ms, 0 unknown).
    pub card_expires: i64,
    /// Renewal charges refused in a row.
    pub failures: i64,
}

/// Renewal charges before auto-renewal stops; one a day, from a day before the cycle ends.
pub const RENEW_TRIES: i64 = 3;
/// While those tries run, the subscription stays on past its end.
pub const RENEW_GRACE_MS: i64 = 3 * crate::plans::DAY_MS;

/// The plan of an account that uses Badi without limits (the owner's, set by email): never refused,
/// never charged, buys nothing.
pub const UNLIMITED: &str = "owner";

/// How far past zero a task that began with credits may still go (milli-credits). Beyond it every call
/// is refused, task or not, until the debt is paid.
pub const OVERDRAFT: i64 = 5 * MILLI;
/// Paid calls one account may have running at once (parallel calls can't all start on the last credit).
pub const MAX_INFLIGHT: i64 = 4;
/// A running call's slot frees itself after this, should the Worker never release it.
pub const INFLIGHT_MS: i64 = 10 * 60_000;
/// A task id as the app sends it in x-badi-task: 1 to 64 letters, digits, `-` or `_`.
pub fn valid_task(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Account {
    pub fn unlimited(&self) -> bool {
        self.plan == UNLIMITED
    }

    /// The subscription running now, if any (also in the grace while a renewal charge is retried).
    pub fn subscription<'a>(&self, c: &'a Catalog, now: i64) -> Option<&'a Plan> {
        let grace = if self.renewal.auto && self.renewal.failures > 0 { RENEW_GRACE_MS } else { 0 };
        c.get(&self.plan).filter(|p| p.kind == Kind::Subscription && self.plan_until + grace > now)
    }

    /// Whether the saved card should be charged for the next cycle now: auto-renewal is on, the plan is
    /// still offered, and the cycle ends within a day (or ended, during the retries).
    pub fn renewal_due(&self, c: &Catalog, now: i64) -> bool {
        self.renewal.auto && self.renewal.card.is_some() && self.subscription(c, now).is_some_and(Plan::buyable) && self.plan_until - now <= crate::plans::DAY_MS
    }

    /// Why the saved card can't be charged any more, when it can't: its token lapses before the cycle ends.
    pub fn card_lapsed(&self, now: i64) -> bool {
        self.renewal.card_expires > 0 && self.renewal.card_expires < self.plan_until.max(now)
    }

    pub fn left(&self, c: &Catalog, now: i64) -> i64 {
        self.meter.left(self.subscription(c, now).is_some())
    }

    /// What Settings and the notch show.
    pub fn view(&self, c: &Catalog, now: i64) -> Value {
        if self.unlimited() {
            return json!({
                "email": self.email,
                "name": self.name,
                "plan": UNLIMITED,
                "planName": "Tanpa batas",
                "subscribed": false,
                "until": 0,
                "canSubscribe": false,
                "unlimited": true,
                "credits": { "left": "∞", "subscription": 0, "subscriptionTotal": 0, "topup": 0, "low": false, "out": false },
            });
        }
        let sub = self.subscription(c, now);
        let left = self.left(c, now);
        let sub_left = if sub.is_some() { self.meter.sub_left.max(0) } else { 0 };
        // Low: under a fifth of the cycle's credits, or under 5 credits without a subscription.
        let low_at = sub.map(|p| p.credits * MILLI / 5).unwrap_or(5 * MILLI);
        let name = match (sub, self.plan.as_str()) {
            (Some(p), _) => p.name.clone(),
            (None, "beta") => "Beta".into(),
            _ => "Free".into(),
        };
        json!({
            "email": self.email,
            "name": self.name,
            "suspended": self.suspended.is_some(),
            "plan": if sub.is_some() { self.plan.as_str() } else if self.plan == "beta" { "beta" } else { "free" },
            "planName": name,
            "subscribed": sub.is_some(),
            "until": if sub.is_some() { self.plan_until } else { 0 },
            "canSubscribe": sub.is_none() && self.plan != "beta",
            // Card subscriptions only: renews by itself (cancel any time), the card it charges, the
            // charges refused so far, and whether renewing stopped after RENEW_TRIES of them.
            "renewal": {
                "auto": sub.is_some() && self.renewal.auto && self.renewal.card.is_some(),
                "card": self.renewal.card,
                "failures": self.renewal.failures,
                "tries": RENEW_TRIES,
                "stopped": !self.renewal.auto && self.renewal.failures >= RENEW_TRIES,
            },
            "credits": {
                "left": whole(left),
                "subscription": whole(sub_left),
                "subscriptionTotal": sub.map(|p| p.credits).unwrap_or(0),
                "topup": whole(self.meter.topup),
                // Credits owed (a call that ended past zero), repaid first by the next purchase.
                "owed": if left < 0 { (-left + MILLI - 1) / MILLI } else { 0 },
                "low": left < low_at,
                "out": left <= 0,
            },
        })
    }
}

/// A refused call: HTTP status, error type and the message for the person.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub status: u16,
    pub kind: &'static str,
    pub message: String,
    pub extra: Value,
}

/// A refusal from outside the rules (the Worker's own: payments not set up, Midtrans failing).
pub fn refusal(status: u16, kind: &'static str, message: &str) -> Refusal {
    Refusal::new(status, kind, message)
}

impl Refusal {
    fn new(status: u16, kind: &'static str, message: impl Into<String>) -> Refusal {
        Refusal { status, kind, message: message.into(), extra: json!({}) }
    }

    pub fn body(&self) -> Value {
        crate::error_body(self.kind, &self.message, self.extra.clone())
    }
}

/// The refusal for a restricted account.
pub fn suspended() -> Refusal {
    Refusal::new(403, "account_suspended", "Akun aina ini dibatasi. Hubungi tim aina untuk membukanya lagi.")
}

/// May this account start a call? It needs credits left; `continuing`: its task began with credits and
/// may finish, but never more than OVERDRAFT past zero. A restricted account starts nothing.
pub fn admit(a: &Account, c: &Catalog, continuing: bool, now: i64) -> Result<(), Refusal> {
    if a.suspended.is_some() {
        return Err(suspended());
    }
    let left = a.left(c, now);
    if a.unlimited() || left > 0 || (continuing && left > -OVERDRAFT) {
        return Ok(());
    }
    let msg = if a.plan == "beta" {
        "Kredit beta aina sudah habis. Terima kasih sudah mencoba! Kabari kami kalau mau lanjut.".to_string()
    } else if let Some(p) = a.subscription(c, now) {
        format!("Kredit bulanan paket {} sudah habis sampai {}. Beli Top-Up di Settings → Akun untuk lanjut sekarang.", p.name, date(a.plan_until))
    } else {
        "Kredit aina sudah habis. Pilih langganan atau Top-Up di Settings → Akun.".to_string()
    };
    Err(Refusal::new(402, if a.plan == "beta" { "beta_over" } else { "credits_out" }, msg))
}

/// May this account use the work apps (Composio)? They are for paying accounts: a subscription running,
/// or credits left after a paid purchase (`has_paid`). The free starter alone doesn't open them.
pub fn admit_apps(a: &Account, c: &Catalog, has_paid: bool, now: i64) -> Result<(), Refusal> {
    if a.suspended.is_some() {
        return Err(suspended());
    }
    if a.unlimited() || a.subscription(c, now).is_some() || has_paid {
        return Ok(());
    }
    Err(Refusal::new(402, "paid_only", "Aplikasi kerja (Gmail, Google Calendar, Drive…) khusus akun berbayar. Pilih langganan atau Top-Up di Settings → Akun."))
}

/// May this account buy this item now? Subscriptions can't be renewed or switched before the cycle ends
/// (credits running out is what top-ups are for); Enterprise is by contact; the beta buys nothing.
pub fn can_buy<'a>(a: &Account, c: &'a Catalog, item: &str, now: i64) -> Result<&'a Plan, Refusal> {
    if a.suspended.is_some() {
        return Err(suspended());
    }
    if a.plan == "beta" {
        return Err(Refusal::new(403, "plan_limit", "Selama beta, paket dan Top-Up belum bisa dibeli."));
    }
    if a.unlimited() {
        return Err(Refusal::new(403, "plan_limit", "Akun ini sudah tanpa batas, tidak perlu membeli paket."));
    }
    let Some(p) = c.get(item) else { return Err(Refusal::new(400, "invalid_request_error", "Paket tidak dikenal.")) };
    if p.kind == Kind::Enterprise {
        return Err(Refusal::new(400, "invalid_request_error", "Paket Enterprise lewat tim kami: pakai tombol Hubungi Sales."));
    }
    if !p.buyable() {
        return Err(Refusal::new(400, "invalid_request_error", "Paket ini tidak bisa dibeli."));
    }
    if p.kind == Kind::Subscription
        && let Some(cur) = a.subscription(c, now)
    {
        let mut r = Refusal::new(
            409,
            "subscription_active",
            format!("Langganan {} masih aktif sampai {}. Kalau kreditnya habis sebelum itu, beli Top-Up; kreditnya tidak pernah hangus.", cur.name, date(a.plan_until)),
        );
        r.extra = json!({ "until": a.plan_until });
        return Err(r);
    }
    Ok(p)
}

/// What a paid renewal gives: (plan, plan_until, sub_left). The next cycle follows the current one (it
/// was charged up to a day early, or late during the retries) with fresh credits; what was left of the
/// ending cycle would lapse within the day anyway.
pub fn renew(a: &Account, p: &Plan, now: i64) -> (String, i64, i64) {
    (p.id.clone(), a.plan_until.max(now) + p.days.max(1) * crate::plans::DAY_MS, p.credits * MILLI)
}

/// After a refused renewal charge: (failures, whether auto-renewal stops).
pub fn renewal_refused(a: &Account) -> (i64, bool) {
    let f = a.renewal.failures + 1;
    (f, f >= RENEW_TRIES)
}

/// What a paid order gives: (plan, plan_until, sub_left, topup) for the account. A subscription starts
/// a new cycle now with its credits (what was left of a lapsed cycle is gone); a top-up adds credits
/// that never lapse.
pub fn purchase(a: &Account, p: &Plan, now: i64) -> (String, i64, i64, i64) {
    match p.kind {
        Kind::Subscription => (p.id.clone(), now + p.days.max(1) * crate::plans::DAY_MS, p.credits * MILLI, a.meter.topup),
        _ => (a.plan.clone(), a.plan_until, a.meter.sub_left, a.meter.topup + p.credits * MILLI),
    }
}

/// A Midtrans status that takes back a payment already delivered: Some(whether the account is also
/// restricted). A chargeback is the payer disputing it, so the account stops; a refund (or a capture
/// cancelled or denied after the fact) only takes the credits back.
pub fn reversal(status: &str) -> Option<bool> {
    match status {
        "chargeback" | "partial_chargeback" => Some(true),
        "refund" | "partial_refund" | "cancel" | "deny" => Some(false),
        _ => None,
    }
}

/// What a reversed payment of `p` takes back: (sub_left, topup, plan_until). A top-up's credits come off
/// the balance (into debt if already spent); the subscription it paid for ends now, and what its cycle
/// already used becomes debt. A plan the account no longer runs changes nothing.
pub fn claw_back(a: &Account, p: &Plan, now: i64) -> (i64, i64, i64) {
    let (sub, top, until) = (a.meter.sub_left, a.meter.topup, a.plan_until);
    match p.kind {
        Kind::Topup => (sub, top - p.credits * MILLI, until),
        Kind::Subscription if a.plan == p.id && until > now => {
            let used = (p.credits * MILLI - sub.max(0)).max(0);
            (0, top - used, now)
        }
        _ => (sub, top, until),
    }
}

/// A PC's id as the app sends it (a hash of the Windows MachineGuid): 32 to 128 hex characters.
pub fn valid_hwid(s: &str) -> bool {
    (32..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The free starter's milli-credits, when this account may have them: once per account, once per PC,
/// and not on the beta.
pub fn free_grant(a: &Account, c: &Catalog, pc_claimed: bool) -> Option<i64> {
    if a.free_claimed || pc_claimed || a.plan == "beta" {
        return None;
    }
    c.of_kind(Kind::Free).filter(|p| p.active && p.credits > 0).map(|p| p.credits * MILLI)
}

/// The Claude models Badi Cloud pays for.
pub fn check_model(model: &str) -> Result<(), Refusal> {
    if model_allowed(model) { Ok(()) } else { Err(Refusal::new(403, "plan_limit", format!("Model {model} tidak termasuk aina Cloud."))) }
}

/// OpenAI voice models Badi Cloud pays for.
pub fn check_tts_model(model: &str) -> Result<(), Refusal> {
    if ["gpt-4o-mini-tts", "tts-1"].contains(&model) { Ok(()) } else { Err(Refusal::new(403, "plan_limit", "That voice model is not part of aina Cloud.")) }
}

/// What the app asks OpenAI for when it opens a realtime session, made safe to pay for: a model Badi
/// Cloud offers (the beta always gets the mini one), replies capped, and a secret that only opens a
/// session in the next minute.
pub fn realtime_secret_body(body: &Value, beta: bool) -> Result<Value, Refusal> {
    let mut session = body["session"].clone();
    if !session.is_object() {
        return Err(Refusal::new(400, "invalid_request_error", "A realtime session is needed."));
    }
    let model = session["model"].as_str().unwrap_or("");
    if !crate::plans::REALTIME_MODELS.contains(&model) {
        return Err(Refusal::new(403, "plan_limit", format!("Model {model} tidak termasuk aina Cloud.")));
    }
    if beta {
        session["model"] = json!("gpt-realtime-mini");
    }
    let cap = session["max_output_tokens"].as_u64().unwrap_or(REALTIME_MAX_OUTPUT).min(REALTIME_MAX_OUTPUT);
    session["max_output_tokens"] = json!(cap);
    Ok(json!({ "expires_after": { "anchor": "created_at", "seconds": 60 }, "session": session }))
}

pub const REALTIME_MAX_OUTPUT: u64 = 1500;

pub fn unauthorized() -> Refusal {
    Refusal::new(401, "authentication_error", "Masuk ke aina Cloud dulu (Settings → Akun).")
}

/// Reads the usage from a Claude reply (streamed or whole) and prices it.
#[derive(Debug, Default, Clone)]
pub struct UsageMeter {
    buf: String,
    /// input, output, cache read, cache write, web searches
    pub usage: [u64; 5],
}

impl UsageMeter {
    pub fn take(&mut self, u: &Value) {
        for (i, k) in ["input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"].iter().enumerate() {
            if let Some(n) = u[*k].as_u64() {
                self.usage[i] = n;
            }
        }
        if let Some(n) = u["server_tool_use"]["web_search_requests"].as_u64() {
            self.usage[4] = n;
        }
    }

    /// Feeds bytes of a server-sent event stream.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let Ok(ev) = serde_json::from_str::<Value>(data.trim()) else { continue };
            match ev["type"].as_str() {
                Some("message_start") => self.take(&ev["message"]["usage"].clone()),
                Some("message_delta") => self.take(&ev["usage"].clone()),
                _ => {}
            }
        }
    }

    pub fn cost(&self, model: &str) -> i64 {
        let [i, o, r, w, s] = self.usage;
        claude_cost(model, i, o, r, w) + search_cost(s)
    }
}

/// The Composio apps Badi offers.
pub const COMPOSIO_TOOLKITS: [&str; 17] = [
    "gmail", "googlecalendar", "googledrive", "googledocs", "googlesheets", "googletasks", "outlook", "microsoft_teams", "notion", "linear", "jira", "clickup", "hubspot", "salesforce",
    "dropbox", "github", "slack",
];

/// Sign-in as B.A.D.I.: the OAuth app of each provider (registered in its console with Composio's
/// redirect URI) covers these apps. Registered once on Badi Cloud (the admin API), it is what every
/// account's sign-in page shows instead of Composio.
pub const COMPOSIO_PROVIDERS: [(&str, &[&str]); 11] = [
    ("google", &["gmail", "googlecalendar", "googledrive", "googledocs", "googlesheets", "googletasks"]),
    ("microsoft", &["outlook", "microsoft_teams"]),
    ("slack", &["slack"]),
    ("github", &["github"]),
    ("notion", &["notion"]),
    ("linear", &["linear"]),
    ("jira", &["jira"]),
    ("clickup", &["clickup"]),
    ("hubspot", &["hubspot"]),
    ("salesforce", &["salesforce"]),
    ("dropbox", &["dropbox"]),
];

/// Composio's OAuth callback (enter it in each provider's OAuth app).
pub const COMPOSIO_CALLBACK: &str = "https://backend.composio.dev/api/v3/toolkits/auth/callback";

/// Badi Cloud's own callback path: it only 302-redirects the browser to COMPOSIO_CALLBACK with the same
/// query, so the provider sends people back to Badi's domain instead of Composio's.
pub const REDIRECT_PROXY_PATH: &str = "/api/composio-redirect";

/// Where the redirect proxy sends the browser: Composio's callback with the provider's query, as is.
pub fn composio_callback_url(query: &str) -> String {
    if query.is_empty() { COMPOSIO_CALLBACK.to_string() } else { format!("{COMPOSIO_CALLBACK}?{}", query.trim_start_matches('?')) }
}

pub fn provider_slugs(provider: &str) -> Option<&'static [&'static str]> {
    COMPOSIO_PROVIDERS.iter().find(|(p, _)| *p == provider).map(|(_, s)| *s)
}

/// The Composio auth config that signs one app in with B.A.D.I.'s own OAuth app. With `redirect_uri`
/// (Badi Cloud's proxy), the provider returns to Badi's domain; without, to Composio's callback. It is
/// always set: without `oauth_redirect_uri` some toolkits fall back to Composio's old
/// /api/v1/auth-apps/add, which the OAuth app doesn't list (Google: redirect_uri_mismatch).
pub fn brand_auth_config(slug: &str, client_id: &str, client_secret: &str, redirect_uri: Option<&str>) -> Value {
    let credentials = json!({ "client_id": client_id.trim(), "client_secret": client_secret.trim(), "oauth_redirect_uri": redirect_uri.unwrap_or(COMPOSIO_CALLBACK) });
    json!({
        "toolkit": { "slug": slug },
        "auth_config": { "type": "use_custom_auth", "name": format!("aina ({slug})"), "authScheme": "OAUTH2", "credentials": credentials }
    })
}

/// How a Composio call (method, path under /api/v3.1/) is handled for an account.
#[derive(Debug, Clone, PartialEq)]
pub enum ComposioRoute {
    /// A new session: send this body (the account's user id, our apps and sign-in), then own the session.
    NewSession(Value),
    /// A call on a session: allowed only when the account owns it.
    Session(String),
    /// A linked account: allowed only when it is the account's own (checked against Composio).
    LinkedAccount(String),
    /// Passed as is (file staging, a tool's schema).
    Pass,
    /// A direct tool call: send this body (the account's user id).
    Execute(Value),
    /// Not available through Badi Cloud.
    Deny,
}

/// Decides how a Composio call is scoped. `auth_configs`: the server's white-label sign-in, if any.
pub fn composio_route(method: &str, path: &str, body: &Value, a: &Account, auth_configs: &Value) -> ComposioRoute {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let mut body = if body.is_object() { body.clone() } else { json!({}) };
    match (method, parts.as_slice()) {
        ("POST", ["tool_router", "session"]) => {
            body["user_id"] = json!(a.composio_user);
            body["toolkits"] = json!({ "enable": COMPOSIO_TOOLKITS });
            match auth_configs.as_object() {
                Some(o) if !o.is_empty() => body["auth_configs"] = auth_configs.clone(),
                _ => {
                    if let Some(o) = body.as_object_mut() {
                        o.remove("auth_configs");
                    }
                }
            }
            ComposioRoute::NewSession(body)
        }
        (_, ["tool_router", "session", id, ..]) => ComposioRoute::Session(id.to_string()),
        ("GET" | "DELETE", ["connected_accounts", id]) if valid_composio_id(id) => ComposioRoute::LinkedAccount(id.to_string()),
        // A tool run on a linked account names it; it must be the account's own (checked by the Worker).
        ("POST", ["tools", "execute", _]) if body.get("connected_account_id").is_some_and(|v| !v.as_str().is_some_and(valid_composio_id)) => ComposioRoute::Deny,
        ("POST", ["files", "upload", "request"]) | ("GET", ["tools", _]) => ComposioRoute::Pass,
        ("POST", ["tools", "execute", _]) => {
            body["user_id"] = json!(a.composio_user);
            ComposioRoute::Execute(body)
        }
        // White-label sign-in is set up on the server, not per user.
        _ => ComposioRoute::Deny,
    }
}

/// A Composio id (ca_…, trs_…) safe to put in a path: letters, digits, `_` and `-` only.
pub fn valid_composio_id(s: &str) -> bool {
    (1..=100).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn not_found() -> Refusal {
    Refusal::new(404, "not_found", "Not available through aina Cloud.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plans::DAY_MS;

    const SSE: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1400,\"cache_read_input_tokens\":4000,\"cache_creation_input_tokens\":0,\"output_tokens\":1}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Halo\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":200}}\n\n";
    const NOW: i64 = 1_792_398_000_000;

    fn account(plan: &str, until: i64, sub_left: i64, topup: i64) -> Account {
        Account { id: 1, email: "a@b.id".into(), name: None, plan: plan.into(), plan_until: until, composio_user: "badi_x".into(), meter: Meter { sub_left, topup }, free_claimed: false, renewal: Renewal::default(), suspended: None }
    }

    #[test]
    fn a_task_may_finish_only_a_little_past_zero() {
        let c = Catalog::defaults();
        // Past zero, a running task may go on up to the overdraft, never further, and a new one can't start.
        assert!(admit(&account("free", 0, 0, -1_000), &c, true, NOW).is_ok());
        assert!(admit(&account("free", 0, 0, -1_000), &c, false, NOW).is_err());
        assert_eq!(admit(&account("free", 0, 0, -OVERDRAFT), &c, true, NOW).unwrap_err().kind, "credits_out");
        assert_eq!(account("free", 0, 0, -2_500).view(&c, NOW)["credits"]["owed"], 3);
        assert!(valid_task("task-1_a") && !valid_task("") && !valid_task(&"a".repeat(65)) && !valid_task("a b"));
    }

    #[test]
    fn a_restricted_account_is_refused_everything_paid() {
        let c = Catalog::defaults();
        let a = Account { suspended: Some("chargeback".into()), ..account("pro", NOW + DAY_MS, 100_000, 50_000) };
        assert_eq!(admit(&a, &c, true, NOW).unwrap_err().kind, "account_suspended");
        assert!(admit_apps(&a, &c, true, NOW).is_err() && can_buy(&a, &c, "topup_small", NOW).is_err());
        assert_eq!(a.view(&c, NOW)["suspended"], true);
    }

    #[test]
    fn a_reversed_payment_takes_its_credits_back() {
        let c = Catalog::defaults();
        assert_eq!((reversal("chargeback"), reversal("refund"), reversal("settlement"), reversal("expire")), (Some(true), Some(false), None, None));
        // A top-up of 65 credits, 60 already spent: 5 left, minus 65 = 60 owed.
        let (sub, top, _) = claw_back(&account("free", 0, 0, 5_000), c.get("topup_small").unwrap(), NOW);
        assert_eq!((sub, top), (0, -60_000));
        // Pro with 25 of its 225 credits left: the cycle ends now and the 200 used are owed.
        let (sub, top, until) = claw_back(&account("pro", NOW + 9 * DAY_MS, 25_000, 10_000), c.get("pro").unwrap(), NOW);
        assert_eq!((sub, top, until), (0, -190_000, NOW));
        // A cycle that already ended (or another plan) is left alone.
        let a = account("basic", NOW + DAY_MS, 1_000, 0);
        assert_eq!(claw_back(&a, c.get("pro").unwrap(), NOW), (1_000, 0, NOW + DAY_MS));
    }

    #[test]
    fn work_apps_are_for_paying_accounts() {
        let c = Catalog::defaults();
        let r = admit_apps(&account("free", 0, 0, 10_000), &c, false, NOW).unwrap_err();
        assert_eq!((r.status, r.kind), (402, "paid_only"));
        assert!(admit_apps(&account("free", 0, 0, 10_000), &c, true, NOW).is_ok(), "a paid top-up opens them");
        assert!(admit_apps(&account("basic", NOW + DAY_MS, 1, 0), &c, false, NOW).is_ok());
        assert!(admit_apps(&account("basic", NOW - 1, 0, 0), &c, false, NOW).is_err(), "a lapsed subscription doesn't");
        assert!(admit_apps(&account(UNLIMITED, 0, 0, 0), &c, false, NOW).is_ok());
    }

    #[test]
    fn calls_need_credits_and_a_running_task_may_finish() {
        let c = Catalog::defaults();
        assert!(admit(&account("basic", NOW + DAY_MS, 1, 0), &c, false, NOW).is_ok());
        assert!(admit(&account("free", 0, 0, 2_000), &c, false, NOW).is_ok(), "free and top-up credits need no subscription");
        let r = admit(&account("basic", NOW + DAY_MS, 0, 0), &c, false, NOW).unwrap_err();
        assert_eq!((r.status, r.kind), (402, "credits_out"));
        assert!(r.message.contains("Top-Up") && r.message.contains("Basic"), "{}", r.message);
        // A lapsed cycle's credits are gone.
        assert!(admit(&account("pro", NOW - 1, 50_000, 0), &c, false, NOW).is_err());
        assert!(admit(&account("pro", NOW - 1, 0, 0), &c, true, NOW).is_ok(), "a running task may finish");
        let r = admit(&account("beta", 0, 0, 0), &c, false, NOW).unwrap_err();
        assert_eq!(r.kind, "beta_over");
    }

    #[test]
    fn no_early_renewal_but_top_ups_any_time() {
        let c = Catalog::defaults();
        let basic = account("basic", NOW + 10 * DAY_MS, 0, 0);
        let r = can_buy(&basic, &c, "pro", NOW).unwrap_err();
        assert_eq!((r.status, r.kind), (409, "subscription_active"));
        assert!(can_buy(&basic, &c, "basic", NOW).is_err(), "no renewing before the cycle ends");
        assert_eq!(can_buy(&basic, &c, "topup_large", NOW).unwrap().id, "topup_large");
        let lapsed = account("basic", NOW - 1, 3_000, 0);
        assert_eq!(can_buy(&lapsed, &c, "pro", NOW).unwrap().id, "pro");
        assert!(can_buy(&lapsed, &c, "enterprise", NOW).is_err() && can_buy(&lapsed, &c, "free", NOW).is_err() && can_buy(&lapsed, &c, "nope", NOW).is_err());
        assert_eq!(can_buy(&account("beta", 0, 0, 0), &c, "topup_small", NOW).unwrap_err().status, 403);
    }

    #[test]
    fn a_payment_starts_a_cycle_or_adds_credits_that_stay() {
        let c = Catalog::defaults();
        let a = account("basic", NOW - 1, 7_000, 4_000);
        let (plan, until, sub, topup) = purchase(&a, c.get("pro").unwrap(), NOW);
        assert_eq!((plan.as_str(), until, sub, topup), ("pro", NOW + 30 * DAY_MS, 225_000, 4_000), "the lapsed cycle's 7 credits are gone");
        let (plan, until, sub, topup) = purchase(&a, c.get("topup_medium").unwrap(), NOW);
        assert_eq!((plan.as_str(), until, sub, topup), ("basic", NOW - 1, 7_000, 214_000));
    }

    #[test]
    fn the_free_starter_comes_once_per_account_and_per_pc() {
        let c = Catalog::defaults();
        let fresh = account("free", 0, 0, 0);
        assert_eq!(free_grant(&fresh, &c, false), Some(10_000));
        assert_eq!(free_grant(&fresh, &c, true), None, "this PC already had it");
        assert_eq!(free_grant(&Account { free_claimed: true, ..fresh.clone() }, &c, false), None);
        assert_eq!(free_grant(&account("beta", 0, 0, 0), &c, false), None);
        assert!(valid_hwid(&"ab12".repeat(16)) && !valid_hwid("short") && !valid_hwid(&"zz".repeat(20)));
    }

    #[test]
    fn the_account_view_shows_credits() {
        let c = Catalog::defaults();
        let v = account("pro", NOW + DAY_MS, 30_500, 12_000).view(&c, NOW);
        assert_eq!((v["planName"].as_str(), v["subscribed"].as_bool(), v["canSubscribe"].as_bool()), (Some("Pro"), Some(true), Some(false)));
        assert_eq!(v["credits"], json!({ "left": 42, "subscription": 30, "subscriptionTotal": 225, "topup": 12, "owed": 0, "low": true, "out": false }));
        let v = account("pro", NOW - 1, 30_500, 0).view(&c, NOW);
        assert_eq!((v["plan"].as_str(), v["planName"].as_str(), v["credits"]["left"].as_i64(), v["credits"]["out"].as_bool()), (Some("free"), Some("Free"), Some(0), Some(true)));
    }

    #[test]
    fn realtime_secrets_are_short_lived_capped_and_for_our_models() {
        let ok = realtime_secret_body(&json!({ "expires_after": { "anchor": "created_at", "seconds": 7200 }, "session": { "type": "realtime", "model": "gpt-realtime", "max_output_tokens": 99_999, "instructions": "hi" } }), false).unwrap();
        assert_eq!(ok["expires_after"]["seconds"], 60);
        assert_eq!(ok["session"]["max_output_tokens"], REALTIME_MAX_OUTPUT);
        assert_eq!(ok["session"]["instructions"], "hi");
        assert_eq!(ok["session"]["model"], "gpt-realtime");
        let beta = realtime_secret_body(&json!({ "session": { "model": "gpt-realtime" } }), true).unwrap();
        assert_eq!(beta["session"]["model"], "gpt-realtime-mini", "the beta is held to the mini model");
        let inf = realtime_secret_body(&json!({ "session": { "model": "gpt-realtime-mini", "max_output_tokens": "inf" } }), false).unwrap();
        assert_eq!(inf["session"]["max_output_tokens"], REALTIME_MAX_OUTPUT);
        assert_eq!(realtime_secret_body(&json!({ "session": { "model": "gpt-5-realtime-preview" } }), false).unwrap_err().status, 403);
        assert_eq!(realtime_secret_body(&json!({}), false).unwrap_err().status, 400);
        assert!(check_model("claude-opus-5").is_ok() && check_model("gpt-5").is_err());
    }

    #[test]
    fn a_streamed_reply_is_priced_from_its_usage_even_cut_off() {
        let mut m = UsageMeter::default();
        // Fed in uneven pieces, as the network delivers them.
        for piece in SSE.as_bytes().chunks(37) {
            m.feed(piece);
        }
        assert_eq!(m.cost("claude-sonnet-5-5"), 100_800);
        let mut cut = UsageMeter::default();
        cut.feed(&SSE.as_bytes()[..SSE.find("content_block_delta").unwrap()]);
        assert_eq!(cut.usage[0], 1_400, "the input is billed even when the app hangs up");
    }

    fn on_card(mut a: Account, failures: i64) -> Account {
        a.renewal = Renewal { auto: true, card: Some("481111-1114".into()), card_expires: 0, failures };
        a
    }

    #[test]
    fn a_card_subscription_renews_from_a_day_before_its_end() {
        let c = Catalog::defaults();
        let a = on_card(account("basic", NOW + 5 * DAY_MS, 0, 0), 0);
        assert!(!a.renewal_due(&c, NOW));
        assert!(a.renewal_due(&c, NOW + 4 * DAY_MS + 1));
        // Cancelled, or paid another way: never charged.
        assert!(!Account { renewal: Renewal { auto: false, ..a.renewal.clone() }, ..a.clone() }.renewal_due(&c, NOW + 5 * DAY_MS));
        assert!(!account("basic", NOW + DAY_MS / 2, 0, 0).renewal_due(&c, NOW));
        // The next cycle follows the current one, with fresh credits.
        let basic = c.get("basic").unwrap();
        let (plan, until, sub_left) = renew(&a, basic, NOW + 4 * DAY_MS + 1);
        assert_eq!((plan.as_str(), until, sub_left), ("basic", NOW + 5 * DAY_MS + basic.days * DAY_MS, basic.credits * MILLI));
    }

    #[test]
    fn refused_renewals_keep_the_plan_in_grace_then_stop() {
        let c = Catalog::defaults();
        let end = NOW + DAY_MS;
        // Without retries running the plan ends on time; while they run it stays on for the grace.
        assert!(on_card(account("basic", end, 0, 0), 0).subscription(&c, end + 1).is_none());
        let retrying = on_card(account("basic", end, 0, 0), 1);
        assert!(retrying.subscription(&c, end + DAY_MS).is_some());
        assert!(retrying.renewal_due(&c, end + DAY_MS));
        assert!(retrying.subscription(&c, end + RENEW_GRACE_MS + 1).is_none());
        assert_eq!(renewal_refused(&on_card(account("basic", end, 0, 0), 0)), (1, false));
        assert_eq!(renewal_refused(&on_card(account("basic", end, 0, 0), RENEW_TRIES - 1)), (RENEW_TRIES, true));
        let stopped = Account { renewal: Renewal { auto: false, failures: RENEW_TRIES, ..retrying.renewal.clone() }, ..retrying };
        assert!(stopped.subscription(&c, end + 1).is_none());
        assert_eq!(stopped.view(&c, NOW)["renewal"]["stopped"], true);
    }

    #[test]
    fn a_card_lapsing_before_the_cycle_ends_cannot_renew_it() {
        let mut a = on_card(account("basic", NOW + DAY_MS, 0, 0), 0);
        a.renewal.card_expires = NOW;
        assert!(a.card_lapsed(NOW));
        a.renewal.card_expires = NOW + 400 * DAY_MS;
        assert!(!a.card_lapsed(NOW));
    }

    #[test]
    fn an_unlimited_account_is_never_refused_and_buys_nothing() {
        let c = Catalog::defaults();
        let a = account(UNLIMITED, 0, 0, 0);
        assert!(admit(&a, &c, false, NOW).is_ok());
        assert!(can_buy(&a, &c, "pro", NOW).is_err());
        let v = a.view(&c, NOW);
        assert_eq!(v["unlimited"], true);
        assert_eq!(v["credits"]["out"], false);
        assert!(admit(&account("free", 0, 0, 0), &c, false, NOW).is_err());
    }

    #[test]
    fn composio_calls_are_scoped_to_the_account() {
        let a = account("pro", NOW + DAY_MS, 0, 0);
        let mine = json!({ "gmail": "ac_badi" });
        let ComposioRoute::NewSession(b) = composio_route("POST", "tool_router/session", &json!({ "user_id": "victim", "toolkits": { "enable": ["all"] }, "auth_configs": { "gmail": "ac_evil" } }), &a, &mine) else { panic!() };
        assert_eq!(b["user_id"], "badi_x");
        assert_eq!(b["toolkits"]["enable"].as_array().unwrap().len(), 17);
        assert_eq!(b["auth_configs"]["gmail"], "ac_badi");
        let ComposioRoute::NewSession(b) = composio_route("POST", "tool_router/session", &json!({ "auth_configs": { "gmail": "ac_evil" } }), &a, &json!({})) else { panic!() };
        assert!(b.get("auth_configs").is_none(), "no client-chosen sign-in");
        assert_eq!(composio_route("GET", "tool_router/session/trs_1/tools", &Value::Null, &a, &mine), ComposioRoute::Session("trs_1".into()));
        assert_eq!(composio_route("DELETE", "connected_accounts/ca_1", &Value::Null, &a, &mine), ComposioRoute::LinkedAccount("ca_1".into()));
        let ComposioRoute::Execute(b) = composio_route("POST", "tools/execute/GOOGLEDRIVE_UPLOAD_FILE", &json!({ "user_id": "victim" }), &a, &mine) else { panic!() };
        assert_eq!(b["user_id"], "badi_x");
        let ComposioRoute::Execute(b) = composio_route("POST", "tools/execute/GMAIL_SEND", &json!({ "connected_account_id": "ca_1" }), &a, &mine) else { panic!() };
        assert_eq!(b["connected_account_id"], "ca_1", "the Worker checks it is the account's own");
        assert_eq!(composio_route("POST", "tools/execute/GMAIL_SEND", &json!({ "connected_account_id": "../auth_configs" }), &a, &mine), ComposioRoute::Deny);
        assert_eq!(composio_route("POST", "tools/execute/GMAIL_SEND", &json!({ "connected_account_id": 7 }), &a, &mine), ComposioRoute::Deny);
        assert_eq!(composio_route("GET", "connected_accounts/ca_1%3Fx", &Value::Null, &a, &mine), ComposioRoute::Deny);
        assert_eq!(composio_route("POST", "auth_configs", &json!({}), &a, &mine), ComposioRoute::Deny);
        assert_eq!(composio_route("GET", "connected_accounts", &json!({}), &a, &mine), ComposioRoute::Deny, "no listing other people's accounts");
    }

    #[test]
    fn every_app_has_one_provider_for_signing_in_as_badi() {
        let mut covered: Vec<&str> = COMPOSIO_PROVIDERS.iter().flat_map(|(_, s)| s.iter().copied()).collect();
        covered.sort();
        let mut all = COMPOSIO_TOOLKITS.to_vec();
        all.sort();
        assert_eq!(covered, all, "each app exactly once");
        assert_eq!(provider_slugs("google").unwrap().len(), 6);
        assert!(provider_slugs("composio").is_none());
        let b = brand_auth_config("gmail", " cid ", "sec", None);
        assert_eq!(b["auth_config"]["credentials"]["client_id"], "cid");
        assert_eq!(b["auth_config"]["type"], "use_custom_auth");
        assert_eq!(b["auth_config"]["credentials"]["oauth_redirect_uri"], COMPOSIO_CALLBACK);
        let p = brand_auth_config("gmail", "cid", "sec", Some("https://badi-cloud.faisalmashuri16.workers.dev/api/composio-redirect"));
        assert_eq!(p["auth_config"]["credentials"]["oauth_redirect_uri"],"https://badi-cloud.faisalmashuri16.workers.dev/api/composio-redirect");
        assert_eq!(composio_callback_url("?code=abc&state=xyz"), "https://backend.composio.dev/api/v3/toolkits/auth/callback?code=abc&state=xyz");
        assert_eq!(composio_callback_url(""), COMPOSIO_CALLBACK);
    }
}
