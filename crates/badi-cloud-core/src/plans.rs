// The price list and credits. aina Cloud sells credits: a free starter once per PC, monthly
// subscriptions (Basic, Pro; the cheapest credits, which lapse at the end of each 30-day cycle), top-up
// packs whose credits never lapse, and Enterprise by contact. Every plan lives in the `plans` table, so
// prices and quotas change there without a release; `Catalog::defaults` is the starting price list
// (and what the tests check). A credit stands for a fixed amount of API cost (HPP), Rp 170 by default
// (`billing_settings.rupiah_per_credit`): each call's real cost is converted to credits when it is
// booked. Balances are kept in milli-credits, so changing that rate later never changes what people
// already have. Costs are milli-rupiah at Rp 18,000 per US$ (BRD §6).

use serde_json::{Value, json};

pub const IDR_PER_USD: f64 = 18_000.0;
pub const DAY_MS: i64 = 86_400_000;
/// The default worth of one credit in API cost, rupiah (every row of the price list: HPP / credits).
pub const DEFAULT_RUPIAH_PER_CREDIT: i64 = 170;
/// Balances and bookings are in milli-credits.
pub const MILLI: i64 = 1000;

/// Milli-rupiah.
pub type Cost = i64;

pub const fn rupiah(r: i64) -> Cost {
    r * 1000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The starter credits, once per PC (HWID) and per account; they never lapse.
    Free,
    /// Credits every cycle (`days`), recurring; unused ones lapse when the cycle ends.
    Subscription,
    /// A pack of credits that never lapse.
    Topup,
    /// Custom seats and quota: contact sales.
    Enterprise,
    /// The closed beta's one-off credit.
    Beta,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "free" => Kind::Free,
            "subscription" => Kind::Subscription,
            "topup" => Kind::Topup,
            "enterprise" => Kind::Enterprise,
            "beta" => Kind::Beta,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Kind::Free => "free",
            Kind::Subscription => "subscription",
            Kind::Topup => "topup",
            Kind::Enterprise => "enterprise",
            Kind::Beta => "beta",
        }
    }
}

/// A row of the `plans` table.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// Rupiah (per cycle for a subscription, per pack for a top-up); 0 for free and custom plans.
    pub price: i64,
    /// Credits per cycle or per pack (0 for custom).
    pub credits: i64,
    /// A subscription's cycle in days.
    pub days: i64,
    /// Shown on the plan ("Best Value"); empty for none.
    pub badge: String,
    /// For the team only, never shown ("Decoy", "Target").
    pub note: String,
    pub sort: i64,
    pub active: bool,
}

impl Plan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(id: &str, name: &str, kind: Kind, price: i64, credits: i64, days: i64, badge: &str, note: &str, sort: i64) -> Plan {
        Plan { id: id.into(), name: name.into(), kind, price, credits, days, badge: badge.into(), note: note.into(), sort, active: true }
    }

    /// Can be paid for at checkout.
    pub fn buyable(&self) -> bool {
        self.active && self.price > 0 && self.credits > 0 && matches!(self.kind, Kind::Subscription | Kind::Topup)
    }

    /// Rupiah per credit, rounded ("Rp 573/kredit").
    pub fn per_credit(&self) -> Option<i64> {
        (self.price > 0 && self.credits > 0).then(|| (self.price as f64 / self.credits as f64).round() as i64)
    }

    /// What the app shows (no internal notes).
    pub fn public(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "kind": self.kind.id(), "price": self.price, "credits": self.credits, "days": self.days, "perCredit": self.per_credit(), "badge": self.badge })
    }
}

/// The price list in force: the active and inactive plans, and what a credit is worth.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub plans: Vec<Plan>,
    /// Rupiah of API cost per credit.
    pub rupiah_per_credit: i64,
    /// Where Enterprise is asked about (a mailto: or https: link); empty: no link.
    pub enterprise_contact: String,
}

impl Catalog {
    /// The starting price list (migrations/0003_credits.sql seeds the same rows).
    pub fn defaults() -> Catalog {
        Catalog {
            plans: vec![
                Plan::new("free", "Free", Kind::Free, 0, 10, 0, "", "1x per HWID, subsidi marketing", 0),
                Plan::new("basic", "Basic", Kind::Subscription, 49_000, 85, 30, "", "", 10),
                Plan::new("pro", "Pro", Kind::Subscription, 129_000, 225, 30, "Best Value", "", 20),
                Plan::new("topup_small", "Top-Up Small", Kind::Topup, 49_000, 65, 0, "", "", 30),
                Plan::new("topup_medium", "Top-Up Medium", Kind::Topup, 149_000, 210, 0, "", "Decoy", 40),
                Plan::new("topup_large", "Top-Up Large", Kind::Topup, 199_000, 300, 0, "Paling hemat", "Target", 50),
                Plan::new("enterprise", "Enterprise", Kind::Enterprise, 0, 0, 0, "", "Custom seat & kuota", 60),
                // ≈ US$ 3 of API cost, as the beta credit was before credits.
                Plan { active: false, ..Plan::new("beta", "Beta", Kind::Beta, 0, 318, 0, "", "Beta tertutup", 90) },
            ],
            rupiah_per_credit: DEFAULT_RUPIAH_PER_CREDIT,
            enterprise_contact: String::new(),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Plan> {
        self.plans.iter().find(|p| p.id == id)
    }

    /// The plan of a kind (the first by sort), e.g. the free starter or the beta credit.
    pub fn of_kind(&self, kind: Kind) -> Option<&Plan> {
        self.plans.iter().filter(|p| p.kind == kind).min_by_key(|p| p.sort)
    }

    /// A call's cost in milli-credits (rounded up, so nothing is free by rounding).
    pub fn to_credits(&self, cost: Cost) -> i64 {
        let per = self.rupiah_per_credit.max(1);
        if cost <= 0 { 0 } else { (cost + per - 1) / per }
    }

    /// The price list for the app: subscriptions, top-ups, Enterprise and the free starter.
    pub fn public(&self) -> Value {
        let mut shown: Vec<&Plan> = self.plans.iter().filter(|p| p.active).collect();
        shown.sort_by_key(|p| p.sort);
        let of = |k: Kind| shown.iter().filter(|p| p.kind == k).map(|p| p.public()).collect::<Vec<_>>();
        json!({
            "subscriptions": of(Kind::Subscription),
            "topups": of(Kind::Topup),
            "enterprise": shown.iter().find(|p| p.kind == Kind::Enterprise).map(|p| { let mut v = p.public(); v["contact"] = json!(self.enterprise_contact); v }),
            "free": shown.iter().find(|p| p.kind == Kind::Free).map(|p| p.public()),
            "rupiahPerCredit": self.rupiah_per_credit,
        })
    }
}

/// An account's credits, in milli-credits.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Meter {
    /// What is left of this cycle's subscription credits (lapses when the cycle ends).
    pub sub_left: i64,
    /// Top-up, free and beta credits (never lapse).
    pub topup: i64,
}

impl Meter {
    /// Credits that can be spent now (subscription credits only while the subscription runs). Negative:
    /// the account owes credits (a call that ended past zero), repaid by the next top-up or cycle.
    pub fn left(&self, sub_on: bool) -> i64 {
        (if sub_on { self.sub_left.max(0) } else { 0 }) + self.topup
    }

    /// Books milli-credits: the subscription's first (they lapse), then the rest. What goes past zero is
    /// carried as debt in `topup` (never forgiven), so overshooting the limit can't be free. The Worker
    /// books the same split atomically in SQL (Db::spend); this is its reference.
    pub fn spend(&mut self, credits: i64, sub_on: bool) {
        let mut c = credits.max(0);
        if sub_on {
            let from_sub = c.min(self.sub_left.max(0));
            self.sub_left -= from_sub;
            c -= from_sub;
        }
        self.topup -= c;
    }
}

/// Whole credits for people (rounded down, so the app never shows more than there is).
pub fn whole(milli: i64) -> i64 {
    milli.max(0) / MILLI
}

/// Sonnet, Haiku and Opus: the Claude models aina Cloud pays for (every plan; Opus simply costs more
/// credits).
pub fn model_allowed(model: &str) -> bool {
    ["claude-sonnet-5-5", "claude-haiku-5-5", "claude-opus-5-5", "claude-opus-5"].iter().any(|m| model.starts_with(m))
}

/// A model call's cost from its usage. Cache writes at 1.25× input.
pub fn claude_cost(model: &str, input: u64, output: u64, cache_read: u64, cache_write: u64) -> Cost {
    let (inp, out, read) = match model {
        m if m.starts_with("claude-opus-5-5") => (4.0, 20.0, 0.2),
        m if m.starts_with("claude-opus-5") => (5.0, 25.0, 0.5),
        m if m.starts_with("claude-haiku-5-5") => (0.1, 0.5, 0.01),
        _ => (2.0, 10.0, 0.2), // Sonnet 5.5
    };
    let usd = (input as f64 * inp + output as f64 * out + cache_read as f64 * read + cache_write as f64 * inp * 1.25) / 1e6;
    (usd * IDR_PER_USD * 1000.0).round() as Cost
}

/// Web search: $10 per 1,000 searches.
pub fn search_cost(n: u64) -> Cost {
    (n as f64 * 0.01 * IDR_PER_USD * 1000.0).round() as Cost
}

/// Speech to text: gpt-4o-mini-transcribe at $0.003 a minute, from 16 kHz 16-bit mono audio bytes.
pub fn stt_cost(audio_bytes: usize) -> Cost {
    let minutes = audio_bytes as f64 / 32_000.0 / 60.0;
    (minutes.max(1.0 / 60.0) * 0.003 * IDR_PER_USD * 1000.0).round() as Cost
}

/// Text to speech by characters: OpenAI ±Rp 70 and ElevenLabs ±Rp 200 per 150-character answer.
pub fn tts_cost(chars: usize, elevenlabs: bool) -> Cost {
    (chars as f64 * if elevenlabs { 1.33 } else { 0.47 } * 1000.0).round() as Cost
}

/// The realtime voice models aina Cloud pays for.
pub const REALTIME_MODELS: [&str; 2] = ["gpt-realtime", "gpt-realtime-mini"];

/// One realtime reply from the usage in its `response.done` event: text, audio and images in (cached
/// parts cheaper), text and audio out. Dollars per million tokens as OpenAI prices them.
pub fn realtime_cost(model: &str, u: &Value) -> Cost {
    let (text_in, cached, audio_in, cached_audio, text_out, audio_out) = if model.starts_with("gpt-realtime-mini") { (0.6, 0.06, 10.0, 0.3, 2.4, 20.0) } else { (4.0, 0.4, 32.0, 0.4, 16.0, 64.0) };
    let n = |v: &Value| v.as_u64().unwrap_or(0) as f64;
    let inp = &u["input_token_details"];
    let hit = &inp["cached_tokens_details"];
    let (t_hit, a_hit) = (n(&hit["text_tokens"]), n(&hit["audio_tokens"]));
    let usd = ((n(&inp["text_tokens"]) + n(&inp["image_tokens"]) - t_hit).max(0.0) * text_in
        + t_hit * cached
        + (n(&inp["audio_tokens"]) - a_hit).max(0.0) * audio_in
        + a_hit * cached_audio
        + n(&u["output_token_details"]["text_tokens"]) * text_out
        + n(&u["output_token_details"]["audio_tokens"]) * audio_out)
        / 1e6;
    (usd * IDR_PER_USD * 1000.0).round() as Cost
}

/// "19/10/2026": a date in Western Indonesian Time.
pub fn date(ms: i64) -> String {
    let (y, m, d, _) = civil(ms);
    format!("{d}/{m}/{y}")
}

/// "jam 15.20 WIB (19/10)".
pub fn when(ms: i64) -> String {
    let (_, m, d, sec) = civil(ms);
    format!("jam {:02}.{:02} WIB ({d}/{m})", sec / 3600, sec % 3600 / 60)
}

// (year, month, day, seconds into the day) in WIB.
fn civil(ms: i64) -> (i64, i64, i64, i64) {
    civil_at(ms, 7 * 3600)
}

/// (year, month, day, seconds into the day) at `offset` seconds from UTC.
pub(crate) fn civil_at(ms: i64, offset: i64) -> (i64, i64, i64, i64) {
    let secs = ms.div_euclid(1000) + offset;
    let (day, sec) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_price_list_matches_the_pricing_matrix() {
        let c = Catalog::defaults();
        let row = |id: &str| c.get(id).unwrap();
        // (price, credits, rupiah per credit) as in the matrix.
        for (id, price, credits, per) in [("basic", 49_000, 85, 576), ("pro", 129_000, 225, 573), ("topup_small", 49_000, 65, 754), ("topup_medium", 149_000, 210, 710), ("topup_large", 199_000, 300, 663)] {
            let p = row(id);
            assert_eq!((p.price, p.credits, p.per_credit()), (price, credits, Some(per)), "{id}");
            assert!(p.buyable(), "{id}");
        }
        // HPP per unit = credits × Rp 170, and the gross margins in the matrix.
        let hpp = |id: &str| row(id).credits * c.rupiah_per_credit;
        assert_eq!((hpp("free"), hpp("basic"), hpp("pro"), hpp("topup_small"), hpp("topup_medium"), hpp("topup_large")), (1_700, 14_450, 38_250, 11_050, 35_700, 51_000));
        let margin = |id: &str| ((1.0 - hpp(id) as f64 / row(id).price as f64) * 1000.0).round() / 10.0;
        assert_eq!((margin("basic"), margin("pro"), margin("topup_small"), margin("topup_medium"), margin("topup_large")), (70.5, 70.3, 77.4, 76.0, 74.4));
        // The decoy: Rp 50,000 more than Medium buys 90 more credits.
        assert_eq!((row("topup_large").price - row("topup_medium").price, row("topup_large").credits - row("topup_medium").credits), (50_000, 90));
        assert_eq!(row("pro").badge, "Best Value");
        assert!(!row("free").buyable() && !row("enterprise").buyable() && !row("beta").buyable());
        assert_eq!(row("basic").days, 30);
    }

    #[test]
    fn the_public_list_groups_plans_and_hides_notes() {
        let mut c = Catalog::defaults();
        c.enterprise_contact = "mailto:sales@aina.ai.id".into();
        let v = c.public();
        let ids = |k: &str| v[k].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(ids("subscriptions"), ["basic", "pro"]);
        assert_eq!(ids("topups"), ["topup_small", "topup_medium", "topup_large"]);
        assert_eq!(v["enterprise"]["contact"], "mailto:sales@aina.ai.id");
        assert_eq!(v["free"]["credits"], 10);
        assert!(!v.to_string().contains("Decoy"), "internal notes stay internal");
        assert!(!v.to_string().contains("\"beta\""), "an inactive plan is not offered");
        c.plans.iter_mut().find(|p| p.id == "topup_medium").unwrap().active = false;
        assert_eq!(c.public()["topups"].as_array().unwrap().len(), 2, "a plan switched off in the table disappears");
    }

    #[test]
    fn costs_become_credits_at_the_rate_in_force() {
        let mut c = Catalog::defaults();
        assert_eq!(c.to_credits(rupiah(170)), 1_000, "Rp 170 of API cost is one credit");
        assert_eq!(c.to_credits(100_800), 593, "a cached screen question ≈ 0.6 credit");
        assert_eq!(c.to_credits(1), 1, "rounded up");
        assert_eq!(c.to_credits(0), 0);
        c.rupiah_per_credit = 200;
        assert_eq!(c.to_credits(rupiah(170)), 850);
    }

    #[test]
    fn subscription_credits_go_first_and_top_ups_never_lapse() {
        let mut m = Meter { sub_left: 5_000, topup: 3_000 };
        assert_eq!(m.left(true), 8_000);
        assert_eq!(m.left(false), 3_000, "a lapsed subscription's credits don't count");
        m.spend(6_000, true);
        assert_eq!((m.sub_left, m.topup), (0, 2_000));
        m.spend(2_500, true);
        assert_eq!((m.sub_left, m.topup), (0, -500), "going past zero is owed, not forgiven");
        assert_eq!(m.left(true), -500);
        m.topup += 65_000; // a Top-Up repays the debt first
        assert_eq!(m.left(false), 64_500);
        let mut lapsed = Meter { sub_left: 5_000, topup: 1_000 };
        lapsed.spend(400, false);
        assert_eq!((lapsed.sub_left, lapsed.topup), (5_000, 600));
        assert_eq!(whole(2_999), 2);
    }

    #[test]
    fn costs_match_the_brd_examples() {
        // A cached screen question on Sonnet 5.5: 1.4k in, 200 out, 4k cache read ≈ Rp 101.
        assert_eq!(claude_cost("claude-sonnet-5-5", 1_400, 200, 4_000, 0), 100_800);
        assert_eq!(tts_cost(150, false) / 1000, 70);
        assert_eq!(tts_cost(150, true) / 1000, 199);
        assert_eq!(stt_cost(320_000) / 1000, 9);
        assert_eq!(search_cost(1), 180_000);
        assert!(model_allowed("claude-opus-5") && model_allowed("claude-sonnet-5-5") && !model_allowed("gpt-5"));
    }

    #[test]
    fn realtime_replies_are_priced_by_their_tokens() {
        // A spoken answer: 1,000 audio tokens in (400 cached) + 500 text in, 600 audio tokens out.
        let u = json!({ "input_token_details": { "text_tokens": 500, "audio_tokens": 1_000, "cached_tokens_details": { "text_tokens": 0, "audio_tokens": 400 } }, "output_token_details": { "text_tokens": 0, "audio_tokens": 600 } });
        // (500×4 + 600×32 + 400×0.4 + 600×64) / 1e6 = $0.05976 ≈ Rp 1,076
        assert_eq!(realtime_cost("gpt-realtime", &u), 1_075_680);
        assert!(realtime_cost("gpt-realtime-mini", &u) < realtime_cost("gpt-realtime", &u) / 3);
        assert_eq!(realtime_cost("gpt-realtime", &json!({})), 0);
    }

    #[test]
    fn times_read_in_wib() {
        // 2026-10-19 08:20 UTC = 15.20 WIB.
        assert_eq!(when(1_792_398_000_000), "jam 15.20 WIB (19/10)");
        assert_eq!(date(1_792_398_000_000), "19/10/2026");
    }
}
