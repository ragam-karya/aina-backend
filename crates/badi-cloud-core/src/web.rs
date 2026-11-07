// The Worker's own pages, now only the download placeholder: signing in, the plans and the thanks
// page live on the website (FRONTEND_URL), which calls the JSON API. The look follows the landing page
// (its colours and fonts, light and dark).

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

/// /download before an installer is uploaded.
pub fn download_soon_page() -> String {
    shell("Download · aina", "", "<h1>Segera tersedia</h1><p class=\"muted\">Installer aina untuk Windows sedang disiapkan. Coba lagi sebentar lagi.</p><p><a class=\"btn ghost\" href=\"/langganan\">Lihat paket</a></p>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rupiah_has_thousands_dots() {
        assert_eq!(rupiah(49_000), "Rp49.000");
        assert_eq!(rupiah(1_299_000), "Rp1.299.000");
        assert_eq!(rupiah(170), "Rp170");
    }

    #[test]
    fn the_download_placeholder_is_a_page() {
        assert!(download_soon_page().contains("Segera tersedia"));
    }
}
