// Badi Cloud's rules, without any I/O, so the Cloudflare Worker (cloud/) and native tests share them:
// the price list and credits, login codes and device tokens, Midtrans checks, what a call may do,
// how Composio calls are scoped to an account, and billing a Claude reply from its usage.

pub mod plans;
pub mod rules;
pub mod web;

use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};

// ---------- login (Google, back to the app through badi://) ----------
// The app opens /login?app_state=… in the browser; the Worker sends the person to Google; Google comes
// back to /auth/google/callback; the Worker opens badi://login?code=…&state=… with a one-use code
// (5 minutes); the app trades it for a device token, but only for the app_state it made itself, so a
// link from someone else can't sign the app into their account. Only hashes of codes and tokens are kept.

pub const OAUTH_STATE_TTL_MS: i64 = 10 * 60_000;
pub const GRANT_TTL_MS: i64 = 5 * 60_000;
pub const TOKEN_PREFIX: &str = "bct_";
const GOOGLE_AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_TOKEN: &str = "https://oauth2.googleapis.com/token";

pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::getrandom(&mut b).expect("no randomness");
    hex::encode(b)
}

pub fn sha256(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Constant-time compare of two hashes.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn new_token() -> String {
    format!("{TOKEN_PREFIX}{}", random_hex(32))
}

/// An email as the beta invite list keeps it: trimmed, lower case, and plausibly an address.
pub fn clean_email(s: &str) -> Option<String> {
    let e = s.trim().to_lowercase();
    let (user, domain) = e.split_once('@')?;
    let ok = !user.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && e.len() <= 254 && !e.chars().any(|c| c.is_whitespace() || c == ',' || c == ';');
    ok.then_some(e)
}

/// The admin API key in `Authorization: Bearer …` matches ADMIN_TOKEN (never when that is unset).
pub fn admin_ok(header: Option<&str>, token: &str) -> bool {
    let given = header.and_then(|h| h.strip_prefix("Bearer ")).unwrap_or("").trim();
    token.len() >= 24 && same(given, token)
}

/// The app's state: 32 to 128 letters and digits (the app makes 64 hex characters).
pub fn valid_app_state(s: &str) -> bool {
    (32..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(crate) fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Google's sign-in page for this login.
pub fn google_auth_url(client_id: &str, redirect_uri: &str, state: &str) -> String {
    format!(
        "{GOOGLE_AUTH}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&prompt=select_account",
        enc(client_id),
        enc(redirect_uri),
        enc("openid email"),
        enc(state)
    )
}

/// The form body that trades Google's code for its tokens.
pub fn google_token_form(client_id: &str, secret: &str, redirect_uri: &str, code: &str) -> String {
    format!("code={}&client_id={}&client_secret={}&redirect_uri={}&grant_type=authorization_code", enc(code), enc(client_id), enc(secret), enc(redirect_uri))
}

/// The verified email in Google's ID token for our client, or None. The token came straight from
/// Google's token endpoint over TLS with our secret, so its claims are read without re-checking the
/// signature (OpenID Connect Core §3.1.3.7).
pub fn google_email(id_token: &str, client_id: &str) -> Option<String> {
    use base64::Engine;
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let issuer_ok = matches!(claims["iss"].as_str(), Some("https://accounts.google.com" | "accounts.google.com"));
    let verified = claims["email_verified"] == true || claims["email_verified"] == "true";
    (issuer_ok && claims["aud"] == client_id && verified).then(|| claims["email"].as_str().map(|e| e.trim().to_lowercase())).flatten()
}

/// The page that hands the login back to the app.
pub fn back_to_app_page(code: &str, app_state: &str) -> String {
    let link = format!("badi://login?code={}&state={}", enc(code), enc(app_state));
    format!(
        "<!doctype html><html lang=\"id\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><link href=\"https://fonts.googleapis.com/css2?family=Urbanist:wght@400;600;700&amp;display=swap\" rel=\"stylesheet\"><title>Masuk ke aina</title><body style=\"font-family:Urbanist,system-ui,'Segoe UI',sans-serif;max-width:520px;margin:0 auto;padding:48px 16px;line-height:1.6\"><h1>Berhasil masuk</h1><p>Kembali ke aina … Kalau browser bertanya, pilih <b>Buka aina</b></p><p><a href=\"{link}\" style=\"display:inline-block;padding:10px 18px;border-radius:8px;background:#2b8aff;color:#fff;text-decoration:none\">Buka aina</a></p><p style=\"color:#667\">Link ini sekali pakai dan berlaku 5 menit. Jendela ini boleh ditutup setelah aina terbuka.</p><script>location.href={link:?};</script></body></html>"
    )
}

/// Text made safe for HTML (and attribute values in double quotes).
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

// ---------- the web session (subscribing from the browser, without the app) ----------
// /login?next=/langganan signs the browser in instead of the app: the callback sets an HttpOnly cookie
// holding a device token like the app's (label "web") and goes back to `next`, on this site only.

pub const SESSION_COOKIE: &str = "badi_session";
const SESSION_MAX_AGE_S: i64 = 30 * 86_400;

/// Where to go after a web login: a path on this site, or None (`//host`, a scheme or `\` would leave it).
pub fn safe_next(next: &str) -> Option<String> {
    let ok = next.starts_with('/') && !next.starts_with("//") && !next.contains('\\') && !next.contains("://") && next.len() <= 200 && !next.chars().any(char::is_control);
    ok.then(|| next.to_string())
}

/// A cookie's value from a Cookie header.
pub fn cookie(header: Option<&str>, name: &str) -> Option<String> {
    header?.split(';').find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == name && !v.is_empty()).then(|| v.to_string())
    })
}

pub fn session_cookie(token: &str) -> String {
    format!("{SESSION_COOKIE}={token}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={SESSION_MAX_AGE_S}")
}

pub fn clear_cookie() -> String {
    format!("{SESSION_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
}

/// A form posted with the session cookie came from this site (the Origin header, which browsers set on
/// every POST): a page elsewhere can't buy or sign out for the person.
pub fn same_origin(origin: Option<&str>, public_url: &str) -> bool {
    origin.is_some_and(|o| o.trim_end_matches('/').eq_ignore_ascii_case(public_url.trim_end_matches('/')))
}

/// A plain page for login problems.
pub fn login_problem_page(message: &str) -> String {
    let safe = esc(message);
    format!("<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><link href=\"https://fonts.googleapis.com/css2?family=Urbanist:wght@400;600;700&amp;display=swap\" rel=\"stylesheet\"><title>aina</title><body style=\"font-family:Urbanist,system-ui;max-width:520px;margin:0 auto;padding:48px 16px\"><h1>Belum bisa masuk</h1><p>{safe}</p><p>Tutup jendela ini lalu coba masuk lagi.</p>")
}

// ---------- payments (Midtrans Snap) ----------

/// What an order is called on the payment page and receipt (Midtrans allows 50 characters).
pub fn item_name(p: &plans::Plan) -> String {
    let name = match p.kind {
        plans::Kind::Subscription => format!("aina {} ({} hari, {} kredit)", p.name, p.days, p.credits),
        _ => format!("aina {} ({} kredit)", p.name, p.credits),
    };
    name.chars().take(50).collect()
}

/// The Snap transaction for an order of a plan from the price list. A subscription asks Midtrans to
/// keep the card (`card_owner`, the account's id there) when it is paid by card, so it can renew; the
/// card itself stays with Midtrans and Badi only gets its token.
pub fn snap_order(order: &str, p: &plans::Plan, email: &str, finish_url: &str, card_owner: &str) -> Value {
    let mut o = json!({
        "transaction_details": { "order_id": order, "gross_amount": p.price },
        "item_details": [{ "id": p.id, "price": p.price, "quantity": 1, "name": item_name(p) }],
        "customer_details": { "email": email },
        "callbacks": { "finish": finish_url },
    });
    if p.kind == plans::Kind::Subscription {
        o["credit_card"] = json!({ "secure": true, "save_card": true });
        o["user_id"] = json!(card_owner);
    }
    o
}

/// A renewal: the saved card charged for the plan, without the person (no CVV; the card passed 3-D
/// Secure when it was saved).
pub fn renewal_charge(order: &str, p: &plans::Plan, email: &str, card_token: &str) -> Value {
    json!({
        "payment_type": "credit_card",
        "transaction_details": { "order_id": order, "gross_amount": p.price },
        "item_details": [{ "id": p.id, "price": p.price, "quantity": 1, "name": item_name(p) }],
        "customer_details": { "email": email },
        "credit_card": { "token_id": card_token },
    })
}

pub fn snap_url(production: bool) -> &'static str {
    if production { "https://app.midtrans.com/snap/v1/transactions" } else { "https://app.sandbox.midtrans.com/snap/v1/transactions" }
}

/// Midtrans's Core API (status and charges).
pub fn core_api(production: bool) -> &'static str {
    if production { "https://api.midtrans.com" } else { "https://api.sandbox.midtrans.com" }
}

/// The card a subscription was paid with, when Midtrans kept it: (token, masked number, token expiry ms).
/// Only credit card payments carry one; QRIS, virtual accounts and e-wallets never renew.
pub fn saved_card(n: &Value) -> Option<(String, String, i64)> {
    if n["payment_type"] != "credit_card" {
        return None;
    }
    let token = n["saved_token_id"].as_str().filter(|t| !t.is_empty())?;
    let expires = n["saved_token_id_expired_at"].as_str().and_then(midtrans_time).unwrap_or(0);
    Some((token.to_string(), n["masked_card"].as_str().unwrap_or("").to_string(), expires))
}

/// "2026-12-31 07:00:00" in Jakarta time (Midtrans's clock) as epoch ms.
pub fn midtrans_time(s: &str) -> Option<i64> {
    let (d, t) = s.trim().split_once(' ').unwrap_or((s.trim(), "00:00:00"));
    let mut d = d.split('-').map(|x| x.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = t.split(':').map(|x| x.parse::<i64>().unwrap_or(0));
    let (h, mi, sec) = (t.next().unwrap_or(0), t.next().unwrap_or(0), t.next().unwrap_or(0));
    // Days from 1970-01-01 (Howard Hinnant's days_from_civil).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h - 7) * 60 + mi) * 60_000 + sec * 1000)
}

/// A notification is Midtrans's: signature_key = SHA512(order_id + status_code + gross_amount + server_key).
pub fn signature_ok(n: &Value, server_key: &str) -> bool {
    let s = |k: &str| n[k].as_str().unwrap_or("").to_string();
    let want = hex::encode(Sha512::digest(format!("{}{}{}{server_key}", s("order_id"), s("status_code"), s("gross_amount")).as_bytes()));
    !server_key.is_empty() && same(&want, &s("signature_key"))
}

#[derive(Debug, PartialEq)]
pub enum Paid {
    Yes,
    Pending,
    Failed(String),
}

/// What a notification says happened to the payment.
pub fn payment_outcome(n: &Value) -> Paid {
    match (n["transaction_status"].as_str().unwrap_or(""), n["fraud_status"].as_str().unwrap_or("accept")) {
        ("settlement", _) | ("capture", "accept") => Paid::Yes,
        ("pending", _) | ("capture", "challenge") => Paid::Pending,
        (s, _) => Paid::Failed(s.to_string()),
    }
}

/// Midtrans sends the amount as "149000.00".
pub fn amount_of(n: &Value) -> i64 {
    n["gross_amount"].as_str().and_then(|a| a.split('.').next()?.parse().ok()).unwrap_or(-1)
}

/// An error in the Messages API's shape, so the app shows its message as it does Anthropic's.
pub fn error_body(kind: &str, message: &str, extra: Value) -> Value {
    let mut body = json!({ "type": "error", "error": { "type": kind, "message": message } });
    if let Some(o) = extra.as_object() {
        for (k, v) in o {
            body["error"][k.as_str()] = v.clone();
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_emails_are_cleaned_and_the_admin_key_checked() {
        assert_eq!(clean_email(" Sari@Gmail.COM ").as_deref(), Some("sari@gmail.com"));
        for bad in ["", "sari", "sari@gmail", "@gmail.com", "a b@x.id", "a@x.id,b@y.id", "a@.id"] {
            assert!(clean_email(bad).is_none(), "{bad}");
        }
        let key = "k".repeat(32);
        assert!(admin_ok(Some(&format!("Bearer {key}")), &key));
        assert!(!admin_ok(Some("Bearer nope"), &key));
        assert!(!admin_ok(None, &key));
        assert!(!admin_ok(Some("Bearer short"), "short"), "a short ADMIN_TOKEN turns the API off");
    }

    fn note(status: &str, key: &str) -> Value {
        let sig = hex::encode(Sha512::digest(format!("o-1200149000.00{key}").as_bytes()));
        json!({ "order_id": "o-1", "status_code": "200", "gross_amount": "149000.00", "signature_key": sig, "transaction_status": status, "fraud_status": "accept" })
    }

    #[test]
    fn google_login_pieces() {
        use base64::Engine;
        assert!(new_token().starts_with("bct_") && new_token().len() == 4 + 64);
        assert_ne!(new_token(), new_token());
        assert!(same(&sha256("x"), &sha256("x")) && !same(&sha256("x"), &sha256("y")));
        assert!(valid_app_state(&random_hex(32)) && !valid_app_state("short") && !valid_app_state(&format!("{}<", random_hex(20))));
        let url = google_auth_url("cid.apps", "https://w.dev/auth/google/callback", "st");
        assert!(url.contains("redirect_uri=https%3A%2F%2Fw.dev%2Fauth%2Fgoogle%2Fcallback") && url.contains("scope=openid%20email"));
        let jwt = |claims: Value| format!("h.{}.s", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string()));
        let good = json!({ "iss": "https://accounts.google.com", "aud": "cid.apps", "email": "Sari@Gmail.com", "email_verified": true });
        assert_eq!(google_email(&jwt(good.clone()), "cid.apps").as_deref(), Some("sari@gmail.com"));
        assert_eq!(google_email(&jwt(good.clone()), "other"), None, "a token for another client");
        let mut unverified = good.clone();
        unverified["email_verified"] = json!(false);
        assert_eq!(google_email(&jwt(unverified), "cid.apps"), None);
        let mut forged = good;
        forged["iss"] = json!("https://evil.example");
        assert_eq!(google_email(&jwt(forged), "cid.apps"), None);
        let page = back_to_app_page("abc", "st1");
        assert!(page.contains("badi://login?code=abc&state=st1"));
        assert!(login_problem_page("<x>").contains("&lt;x&gt;"));
    }

    #[test]
    fn payments_are_verified_and_read() {
        assert!(signature_ok(&note("settlement", "SB-key"), "SB-key"));
        assert!(!signature_ok(&note("settlement", "SB-key"), "other"));
        assert!(!signature_ok(&note("settlement", ""), ""));
        assert_eq!(payment_outcome(&note("settlement", "k")), Paid::Yes);
        assert_eq!(payment_outcome(&note("pending", "k")), Paid::Pending);
        assert_eq!(payment_outcome(&note("expire", "k")), Paid::Failed("expire".into()));
        assert_eq!(amount_of(&note("settlement", "k")), 149_000);
        let c = plans::Catalog::defaults();
        let pro = snap_order("o", c.get("pro").unwrap(), "a@b.id", "https://x/t", "badi-7");
        assert_eq!(pro["transaction_details"]["gross_amount"], 129_000);
        assert_eq!(pro["item_details"][0]["name"], "aina Pro (30 hari, 225 kredit)");
        assert_eq!(item_name(c.get("topup_large").unwrap()), "aina Top-Up Large (300 kredit)");
        // A subscription keeps the card (to renew); a top-up never does.
        assert_eq!(pro["credit_card"]["save_card"], true);
        assert_eq!(pro["user_id"], "badi-7");
        let top = snap_order("o", c.get("topup_small").unwrap(), "a@b.id", "https://x/t", "badi-7");
        assert!(top.get("credit_card").is_none() && top.get("user_id").is_none());
        let charge = renewal_charge("r", c.get("pro").unwrap(), "a@b.id", "481111xTOKEN1114");
        assert_eq!(charge["credit_card"]["token_id"], "481111xTOKEN1114");
        assert_eq!(charge["payment_type"], "credit_card");
    }

    #[test]
    fn a_web_login_goes_back_only_to_a_page_of_this_site() {
        assert_eq!(safe_next("/langganan"), Some("/langganan".into()));
        assert_eq!(safe_next("/terima-kasih?order_id=x"), Some("/terima-kasih?order_id=x".into()));
        for bad in ["//evil.com", "https://evil.com", "/\\evil.com", "langganan", "/x?u=https://evil.com", "/a\nb"] {
            assert_eq!(safe_next(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_session_cookie_is_read_and_checked() {
        assert_eq!(cookie(Some("a=1; badi_session=bct_abc; b=2"), SESSION_COOKIE), Some("bct_abc".into()));
        assert_eq!(cookie(Some("badi_session="), SESSION_COOKIE), None);
        assert_eq!(cookie(None, SESSION_COOKIE), None);
        let c = session_cookie("bct_x");
        assert!(c.contains("HttpOnly") && c.contains("Secure") && c.contains("SameSite=Lax"));
        assert!(clear_cookie().contains("Max-Age=0"));
        assert!(same_origin(Some("https://badi-cloud.x.workers.dev"), "https://badi-cloud.x.workers.dev/"));
        assert!(!same_origin(Some("https://evil.com"), "https://badi-cloud.x.workers.dev"));
        assert!(!same_origin(None, "https://badi-cloud.x.workers.dev"));
    }

    #[test]
    fn only_a_card_payment_brings_a_card_to_renew_with() {
        let card = json!({ "payment_type": "credit_card", "saved_token_id": "481111xTOKEN1114", "saved_token_id_expired_at": "2030-12-31 07:00:00", "masked_card": "48111111-1114" });
        let (token, mask, expires) = saved_card(&card).unwrap();
        assert_eq!((token.as_str(), mask.as_str()), ("481111xTOKEN1114", "48111111-1114"));
        assert_eq!(expires, 1_924_905_600_000); // 2030-12-31 00:00 UTC
        assert_eq!(saved_card(&json!({ "payment_type": "qris", "saved_token_id": "x" })), None);
        assert_eq!(saved_card(&json!({ "payment_type": "bank_transfer" })), None);
        assert_eq!(saved_card(&json!({ "payment_type": "credit_card" })), None);
        assert_eq!(midtrans_time("1970-01-01 07:00:00"), Some(0));
    }
}
