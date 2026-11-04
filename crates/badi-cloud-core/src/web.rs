// The pages for subscribing from the browser, without the app: the plans (/langganan), after paying
// (/terima-kasih) and the download placeholder. Plain HTML forms, no script; the look follows the
// landing page (its colours and fonts, light and dark).

use serde_json::Value;

use crate::esc;

const STYLE: &str = r#"
:root{color-scheme:light dark;--blue:#2B8AFF;--blue-deep:#0B4FB3;--bg:#F5F8FD;--surface:#FFFFFF;--ink:#0C1A2E;--muted:#52627A;--line:#D8E2F0;--accent:#0B4FB3;--ok:#1E9E5A;--warn:#C8541B}
@media (prefers-color-scheme:dark){:root{--bg:#060B14;--surface:#0D1626;--ink:#E7EFFB;--muted:#93A3BC;--line:#1E2B42;--accent:#7DB8FF;--ok:#4CD38A;--warn:#FF9F6B}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);font:16px/1.6 Urbanist,"Segoe UI Variable Text","Segoe UI",system-ui,sans-serif}
main{max-width:960px;margin:0 auto;padding:32px 16px 64px}
header{display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap;margin-bottom:28px}
.brand{font:800 22px/1 Urbanist,system-ui,sans-serif;color:var(--ink);text-decoration:none;letter-spacing:.02em}
h1,h2{font-family:Urbanist,system-ui,sans-serif;line-height:1.25}
h1{font-size:30px;margin:0 0 8px}h2{font-size:20px;margin:36px 0 12px}
.muted{color:var(--muted)}
.card{background:var(--surface);border:1px solid var(--line);border-radius:16px;padding:20px}
.me{display:flex;justify-content:space-between;align-items:center;gap:16px;flex-wrap:wrap}
.credits{font:800 28px/1 Urbanist,system-ui,sans-serif}
.grid{display:grid;gap:14px;grid-template-columns:repeat(auto-fill,minmax(220px,1fr))}
.plan{display:flex;flex-direction:column;gap:6px}
.plan .name{display:flex;justify-content:space-between;align-items:center;gap:8px;font-weight:700}
.badge{font-size:12px;font-weight:700;padding:2px 8px;border-radius:999px;background:var(--blue);color:#fff}
.price{font:800 24px/1.2 Urbanist,system-ui,sans-serif}
.plan form{margin-top:auto;padding-top:10px}
button,.btn{display:inline-block;font:600 15px/1 inherit;padding:12px 18px;border-radius:10px;border:1px solid transparent;background:var(--blue);color:#fff;text-decoration:none;cursor:pointer;text-align:center}
.btn.google{display:inline-flex;align-items:center;gap:12px;background:#fff;color:#1F1F1F;border:1px solid #747775;font-family:Roboto,"Segoe UI",system-ui,sans-serif;font-weight:500;padding:11px 20px;border-radius:999px}
.btn.google:hover{background:#F2F2F2}
.btn.ghost,button.ghost{background:transparent;color:var(--accent);border-color:var(--line)}
button[disabled]{opacity:.45;cursor:not-allowed}
.plan button{width:100%}
.note{font-size:14px;color:var(--muted)}
.alert{border-radius:12px;padding:12px 16px;margin:16px 0;border:1px solid var(--line);background:var(--surface)}
.alert.err{border-color:var(--warn);color:var(--warn)}
.alert.ok{border-color:var(--ok);color:var(--ok)}
.actions{display:flex;gap:10px;flex-wrap:wrap;margin:20px 0}
footer{margin-top:40px;font-size:14px;color:var(--muted)}
footer a{color:var(--accent)}
"#;

fn shell(title: &str, head_extra: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"id\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title>{head_extra}<link rel=\"preconnect\" href=\"https://fonts.googleapis.com\"><link href=\"https://fonts.googleapis.com/css2?family=Urbanist:wght@400;500;600;700;800&amp;display=swap\" rel=\"stylesheet\"><style>{STYLE}</style></head><body><main><header><a class=\"brand\" href=\"/langganan\">aina</a></header>{body}<footer><a href=\"/ketentuan\">Ketentuan</a> · <a href=\"/privasi\">Privasi</a></footer></main></body></html>",
        esc(title)
    )
}

/// A value for a URL query (a message carried back to /langganan).
pub fn query_value(s: &str) -> String {
    crate::enc(s)
}

/// "Rp49.000".
pub fn rupiah(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    format!("{}Rp{out}", if n < 0 { "-" } else { "" })
}

fn plan_card(p: &Value, per: &str, can_buy: bool, why_not: &str, current: bool) -> String {
    let badge = p["badge"].as_str().filter(|b| !b.is_empty()).map(|b| format!("<span class=\"badge\">{}</span>", esc(b))).unwrap_or_default();
    let price = p["price"].as_i64().unwrap_or(0);
    let per_credit = p["perCredit"].as_i64().map(|r| format!(" · {}/kredit", rupiah(r))).unwrap_or_default();
    let button = if current {
        "<button disabled>Paket aktif</button>".to_string()
    } else if can_buy {
        let label = if p["kind"] == "subscription" { "Langganan" } else { "Beli" };
        format!("<button type=\"submit\">{label}</button>")
    } else {
        format!("<button disabled title=\"{}\">{}</button>", esc(why_not), if p["kind"] == "subscription" { "Langganan" } else { "Beli" })
    };
    format!(
        "<div class=\"card plan\"><div class=\"name\"><span>{}</span>{badge}</div><div class=\"price\">{}<span class=\"note\">{per}</span></div><div>{} kredit<span class=\"note\">{per_credit}</span></div><form method=\"post\" action=\"/web/checkout\"><input type=\"hidden\" name=\"item\" value=\"{}\">{button}</form></div>",
        esc(p["name"].as_str().unwrap_or("")),
        rupiah(price),
        p["credits"].as_i64().unwrap_or(0),
        esc(p["id"].as_str().unwrap_or(""))
    )
}

/// The download and open-the-app buttons, shown after paying and on the plans page.
fn app_actions() -> &'static str {
    "<div class=\"actions\"><a class=\"btn\" href=\"/download\">Download untuk Windows</a><a class=\"btn ghost\" href=\"badi://open\">Buka aplikasi aina</a></div>"
}

/// GET /langganan for a signed-in person: who they are, their plan and credits, and every plan to buy.
/// `account` is Account::view, `plans` Catalog::public, `notice` an error or message to show.
pub fn plans_page(account: &Value, plans: &Value, notice: Option<(&str, bool)>) -> String {
    let unlimited = account["unlimited"] == true;
    let beta = account["plan"] == "beta";
    let subscribed = account["subscribed"] == true;
    let credits = &account["credits"];
    let left = match &credits["left"] {
        Value::String(s) => esc(s),
        v => v.as_i64().unwrap_or(0).to_string(),
    };
    let until = account["untilText"].as_str().map(|d| format!(" · {}", esc(d))).unwrap_or_default();
    let renew = if account["renewal"]["auto"] == true { " · diperpanjang otomatis" } else { "" };
    let mut body = format!(
        "<h1>Pilih paket aina</h1><p class=\"muted\">Langganan dari web, lalu pakai di aplikasi desktop dengan akun Google yang sama.</p><div class=\"card me\"><div><b>{}</b><div class=\"muted\">Paket {}{until}{renew}</div></div><div><span class=\"credits\">{left}</span> <span class=\"muted\">kredit</span></div></div>",
        esc(account["email"].as_str().unwrap_or("")),
        esc(account["planName"].as_str().unwrap_or("Free")),
    );
    if let Some((text, error)) = notice {
        body.push_str(&format!("<div class=\"alert {}\">{}</div>", if error { "err" } else { "ok" }, esc(text)));
    }
    if unlimited {
        body.push_str("<div class=\"alert ok\">Akun ini tanpa batas, jadi tidak perlu membeli paket.</div>");
    } else if beta {
        body.push_str("<div class=\"alert\">Selama beta, paket dan Top-Up belum bisa dibeli.</div>");
    } else {
        let can_sub = account["canSubscribe"] == true;
        let why = if subscribed { "Langganan masih aktif sampai akhir siklusnya. Kalau kredit habis, beli Top-Up." } else { "" };
        body.push_str("<h2>Langganan bulanan</h2><p class=\"note\">Kredit termurah, siklus 30 hari; sisa kredit hangus di akhir siklus. Bayar dengan kartu kredit: diperpanjang otomatis dan bisa dibatalkan kapan saja. QRIS, transfer bank, atau e-wallet: sekali bayar.</p><div class=\"grid\">");
        for p in plans["subscriptions"].as_array().into_iter().flatten() {
            let current = subscribed && account["plan"] == p["id"];
            body.push_str(&plan_card(p, "/bln", can_sub, why, current));
        }
        body.push_str("</div>");
        if subscribed {
            body.push_str(&format!("<p class=\"note\">{why}</p>"));
        }
        body.push_str("<h2>Top-Up</h2><p class=\"note\">Kredit tambahan yang tidak pernah hangus selama akun aktif.</p><div class=\"grid\">");
        for p in plans["topups"].as_array().into_iter().flatten() {
            body.push_str(&plan_card(p, "/pack", true, "", false));
        }
        body.push_str("</div>");
        if let Some(e) = plans["enterprise"].as_object() {
            let contact = e.get("contact").and_then(Value::as_str).filter(|c| !c.is_empty());
            let button = match contact {
                Some(c) => format!("<a class=\"btn ghost\" href=\"{}\">Hubungi Sales</a>", esc(c)),
                None => "<span class=\"note\">Hubungi tim aina.</span>".to_string(),
            };
            body.push_str(&format!("<h2>Enterprise</h2><div class=\"card me\"><div><b>Untuk tim dan perusahaan</b><div class=\"muted\">Seat dan kuota custom.</div></div>{button}</div>"));
        }
    }
    if let Some(free) = plans["free"]["credits"].as_i64().filter(|c| *c > 0) {
        body.push_str(&format!("<p class=\"note\">Setiap PC juga dapat {free} kredit gratis sekali saat pertama masuk dari aplikasi desktop.</p>"));
    }
    body.push_str("<h2>Aplikasi desktop</h2><p class=\"muted\">Sudah punya aplikasinya? Buka lalu masuk dengan akun Google yang sama; paketmu sudah ada di sana.</p>");
    body.push_str(app_actions());
    body.push_str("<form method=\"post\" action=\"/web/logout\"><button class=\"ghost\" type=\"submit\">Keluar</button></form>");
    shell("Paket aina", "", &body)
}

/// What /terima-kasih says about the order.
pub enum Thanks<'a> {
    /// Paid: the plan now active and the credits left.
    Paid { plan: &'a str, credits: &'a str },
    /// Not settled yet: the page looks again in a few seconds (up to `tries_left` more times).
    Checking { order: &'a str, tries_left: u32 },
    /// Refused, cancelled or expired at Midtrans.
    Failed,
    /// No session here (the app's own checkout) or not this person's order.
    Generic,
}

pub fn thanks_page(t: Thanks) -> String {
    let (head, body) = match t {
        Thanks::Paid { plan, credits } => (
            String::new(),
            format!("<h1>Terima kasih!</h1><div class=\"alert ok\">Paket {} aktif · {} kredit.</div><p>Sekarang pasang aplikasi aina di PC Windows-mu, lalu masuk dengan akun Google yang sama.</p>", esc(plan), esc(credits)),
        ),
        Thanks::Checking { order, tries_left } => (
            if tries_left > 0 { format!("<meta http-equiv=\"refresh\" content=\"5;url=/terima-kasih?order_id={}&amp;n={}\">", crate::enc(order), tries_left - 1) } else { String::new() },
            format!(
                "<h1>Pembayaran sedang dicek</h1><p class=\"muted\">{}</p>",
                if tries_left > 0 { "Halaman ini diperbarui sendiri dalam beberapa detik." } else { "Masih menunggu kabar dari Midtrans. Paketmu aktif otomatis setelah lunas; cek lagi nanti di halaman paket." }
            ),
        ),
        Thanks::Failed => (String::new(), "<h1>Pembayaran belum berhasil</h1><div class=\"alert err\">Pembayarannya ditolak, dibatalkan, atau kedaluwarsa. Tidak ada yang ditagih.</div><p><a class=\"btn ghost\" href=\"/langganan\">Coba lagi</a></p>".to_string()),
        Thanks::Generic => (String::new(), "<h1>Terima kasih!</h1><p>Pembayaranmu sedang dicek. Paketmu aktif dalam beberapa detik setelah lunas.</p>".to_string()),
    };
    let body = format!("{body}{}<p class=\"note\">Sudah punya aplikasinya? Buka lalu masuk dengan akun Google yang sama; paketmu sudah ada di sana. <a href=\"/langganan\">Lihat paket</a></p>", app_actions());
    shell("Terima kasih · aina", &head, &body)
}

/// GET /masuk: sign in before choosing a plan. One "Masuk dengan Google" button (Google's own look),
/// going back to `next` (already checked with safe_next) afterwards.
pub fn login_page(next: &str) -> String {
    let google = "<svg width=\"18\" height=\"18\" viewBox=\"0 0 48 48\" aria-hidden=\"true\"><path fill=\"#EA4335\" d=\"M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z\"/><path fill=\"#4285F4\" d=\"M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z\"/><path fill=\"#FBBC05\" d=\"M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z\"/><path fill=\"#34A853\" d=\"M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z\"/></svg>";
    let body = format!(
        "<div class=\"card\" style=\"max-width:440px;margin:24px auto;text-align:center;padding:32px 24px\"><h1>Masuk ke aina</h1><p class=\"muted\">Masuk dengan akun Google untuk memilih paket. Belum punya akun? Akun dibuat otomatis saat pertama masuk.</p><p><a class=\"btn google\" href=\"/login?next={}\">{google}<span>Masuk dengan Google</span></a></p><p class=\"note\">Dengan masuk, kamu setuju dengan <a href=\"/ketentuan\">Ketentuan Layanan</a> dan <a href=\"/privasi\">Kebijakan Privasi</a>.</p><p class=\"note\">Pakai akun Google yang sama di aplikasi desktop supaya paketmu ikut terbawa.</p></div>",
        crate::enc(next)
    );
    shell("Masuk · aina", "", &body)
}

/// /download before an installer is uploaded.
pub fn download_soon_page() -> String {
    shell("Download · aina", "", "<h1>Segera tersedia</h1><p class=\"muted\">Installer aina untuk Windows sedang disiapkan. Coba lagi sebentar lagi.</p><p><a class=\"btn ghost\" href=\"/langganan\">Lihat paket</a></p>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plans() -> Value {
        crate::plans::Catalog::defaults().public()
    }

    #[test]
    fn rupiah_has_thousands_dots() {
        assert_eq!(rupiah(49_000), "Rp49.000");
        assert_eq!(rupiah(1_299_000), "Rp1.299.000");
        assert_eq!(rupiah(170), "Rp170");
    }

    #[test]
    fn the_plans_page_escapes_and_follows_the_account() {
        let free = json!({ "email": "<x>@a.id", "plan": "free", "planName": "Free", "subscribed": false, "canSubscribe": true, "credits": { "left": 10 } });
        let page = plans_page(&free, &plans(), Some(("Paket <b>", true)));
        assert!(page.contains("&lt;x&gt;@a.id") && !page.contains("<x>@"));
        assert!(page.contains("Paket &lt;b&gt;"));
        assert!(page.contains("name=\"item\" value=\"basic\"") && page.contains("<button type=\"submit\">Langganan</button>"));
        // A running subscription: no second one, top-ups still on sale.
        let sub = json!({ "email": "a@a.id", "plan": "basic", "planName": "Basic", "subscribed": true, "canSubscribe": false, "credits": { "left": 85 } });
        let page = plans_page(&sub, &plans(), None);
        assert!(page.contains("Paket aktif"));
        assert!(!page.contains("<button type=\"submit\">Langganan</button>"));
        assert!(page.contains("<button type=\"submit\">Beli</button>"));
        let owner = json!({ "email": "o@a.id", "plan": "owner", "planName": "Tanpa batas", "unlimited": true, "credits": { "left": "∞" } });
        assert!(!plans_page(&owner, &plans(), None).contains("/web/checkout"));
    }

    #[test]
    fn the_login_page_signs_in_with_google_and_comes_back() {
        let p = login_page("/langganan");
        assert!(p.contains("href=\"/login?next=%2Flangganan\""));
        assert!(p.contains("Masuk dengan Google"));
    }

    #[test]
    fn the_thanks_page_refreshes_only_while_checking() {
        assert!(thanks_page(Thanks::Checking { order: "badi-1-2-ab", tries_left: 3 }).contains("http-equiv=\"refresh\""));
        assert!(!thanks_page(Thanks::Checking { order: "badi-1-2-ab", tries_left: 0 }).contains("http-equiv"));
        let paid = thanks_page(Thanks::Paid { plan: "Basic", credits: "85" });
        assert!(paid.contains("/download") && paid.contains("badi://open"));
    }
}
