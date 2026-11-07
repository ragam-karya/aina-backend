// D1 queries for Badi Cloud: the price list (plans, billing_settings), accounts and their credits,
// Google sign-ins under way, login hand-offs, device tokens, the free starter per PC, tasks allowed to
// finish, payments and Composio session ownership. Only what accounts and billing need.

use badi_cloud_core::plans::{Catalog, Kind, MILLI, Meter, Plan};
use badi_cloud_core::rules::{Account, Renewal};
use serde::Deserialize;
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, Result};

#[derive(Deserialize)]
struct UserRow {
    id: f64,
    email: String,
    name: Option<String>,
    plan: String,
    plan_until: f64,
    composio_user: String,
    sub_left: f64,
    topup: f64,
    free_claimed: f64,
    auto_renew: f64,
    card_mask: Option<String>,
    has_card: f64,
    card_expires: f64,
    renew_failures: f64,
}

impl From<UserRow> for Account {
    fn from(r: UserRow) -> Account {
        Account {
            id: r.id as i64,
            email: r.email,
            name: r.name,
            plan: r.plan,
            plan_until: r.plan_until as i64,
            composio_user: r.composio_user,
            meter: Meter { sub_left: r.sub_left as i64, topup: r.topup as i64 },
            free_claimed: r.free_claimed != 0.0,
            // The token itself is read only where a renewal charges it (renewal_token).
            renewal: Renewal {
                auto: r.auto_renew != 0.0,
                card: (r.has_card != 0.0).then(|| r.card_mask.unwrap_or_default()),
                card_expires: r.card_expires as i64,
                failures: r.renew_failures as i64,
            },
        }
    }
}

/// An order: whose it is, what for, how much, and pending, paid or how it failed.
pub struct Payment {
    pub user: i64,
    pub item: String,
    pub amount: i64,
    pub status: String,
}

const USER: &str = "id, email, name, plan, plan_until, composio_user, sub_left, topup, free_claimed, auto_renew, card_mask, (card_token IS NOT NULL) AS has_card, card_expires, renew_failures";

#[derive(Deserialize)]
struct PlanRow {
    id: String,
    name: String,
    kind: String,
    price: f64,
    credits: f64,
    days: f64,
    badge: String,
    note: String,
    sort: f64,
    active: f64,
}

impl PlanRow {
    fn plan(self) -> Option<Plan> {
        Some(Plan {
            kind: Kind::parse(&self.kind)?,
            id: self.id,
            name: self.name,
            price: self.price as i64,
            credits: self.credits as i64,
            days: self.days as i64,
            badge: self.badge,
            note: self.note,
            sort: self.sort as i64,
            active: self.active != 0.0,
        })
    }
}

// D1 binds JS numbers; milliseconds and milli-credits stay well inside 2^53.
fn n(v: i64) -> JsValue {
    JsValue::from_f64(v as f64)
}

fn s(v: &str) -> JsValue {
    JsValue::from_str(v)
}

pub struct Db(pub D1Database);

impl Db {
    async fn run(&self, sql: &str, args: &[JsValue]) -> Result<()> {
        self.0.prepare(sql).bind(args)?.run().await.map(|_| ())
    }

    async fn first<T: for<'de> Deserialize<'de>>(&self, sql: &str, args: &[JsValue]) -> Result<Option<T>> {
        self.0.prepare(sql).bind(args)?.first::<T>(None).await
    }

    pub async fn user(&self, id: i64) -> Result<Option<Account>> {
        Ok(self.first::<UserRow>(&format!("SELECT {USER} FROM users WHERE id = ?1"), &[n(id)]).await?.map(Into::into))
    }

    // ---- the price list ----

    /// The price list in force: every plan in the table and the billing settings (missing settings keep
    /// their defaults).
    pub async fn catalog(&self) -> Result<Catalog> {
        #[derive(Deserialize)]
        struct Setting {
            key: String,
            value: String,
        }
        let mut c = Catalog::defaults();
        c.plans = self.0.prepare("SELECT id, name, kind, price, credits, days, badge, note, sort, active FROM plans ORDER BY sort").all().await?.results::<PlanRow>()?.into_iter().filter_map(PlanRow::plan).collect();
        for st in self.0.prepare("SELECT key, value FROM billing_settings").all().await?.results::<Setting>()? {
            match st.key.as_str() {
                "rupiah_per_credit" => c.rupiah_per_credit = st.value.trim().parse().ok().filter(|r: &i64| *r > 0).unwrap_or(c.rupiah_per_credit),
                "enterprise_contact" => c.enterprise_contact = st.value,
                _ => {}
            }
        }
        Ok(c)
    }

    /// Adds or changes a plan (the admin API).
    pub async fn put_plan(&self, p: &Plan, now: i64) -> Result<()> {
        self.run(
            "INSERT INTO plans (id, name, kind, price, credits, days, badge, note, sort, active, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET name = ?2, kind = ?3, price = ?4, credits = ?5, days = ?6, badge = ?7, note = ?8, sort = ?9, active = ?10, updated_at = ?11",
            &[s(&p.id), s(&p.name), s(p.kind.id()), n(p.price), n(p.credits), n(p.days), s(&p.badge), s(&p.note), n(p.sort), n(p.active as i64), n(now)],
        )
        .await
    }

    pub async fn setting(&self, key: &str) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct V {
            value: String,
        }
        Ok(self.first::<V>("SELECT value FROM billing_settings WHERE key = ?1", &[s(key)]).await?.map(|v| v.value))
    }

    pub async fn put_setting(&self, key: &str, value: &str) -> Result<()> {
        self.run("INSERT INTO billing_settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", &[s(key), s(value)]).await
    }

    /// The account for an email; a new one starts on Free (its starter credits come with the first
    /// sign-in from a PC that hasn't had them), or, invited to the closed beta, on the beta credit (an
    /// invited account still on Free moves to the beta once). The name Google gives is kept up to date.
    /// Returns (account, new).
    pub async fn find_or_create(&self, email: &str, name: Option<&str>, now: i64, composio_user: &str, beta: bool, unlimited: bool) -> Result<(Account, bool)> {
        let q = format!("SELECT {USER} FROM users WHERE email = ?1");
        let beta_credit = if beta { self.catalog().await?.of_kind(Kind::Beta).map(|p| p.credits * MILLI).unwrap_or(0) } else { 0 };
        if let Some(u) = self.first::<UserRow>(&q, &[s(email)]).await? {
            let mut a: Account = u.into();
            if let Some(name) = name.filter(|nm| a.name.as_deref() != Some(*nm)) {
                self.run("UPDATE users SET name = ?2 WHERE id = ?1", &[n(a.id), s(name)]).await?;
                a.name = Some(name.to_string());
            }
            // On the UNLIMITED_EMAILS list: the owner's plan, whatever the account had before.
            if unlimited && !a.unlimited() {
                self.run("UPDATE users SET plan = ?2, plan_until = 0 WHERE id = ?1", &[n(a.id), s(badi_cloud_core::rules::UNLIMITED)]).await?;
                return Ok((self.user(a.id).await?.ok_or("account gone")?, false));
            }
            if beta && a.plan == "free" {
                self.run("UPDATE users SET plan = 'beta', plan_until = 0, topup = topup + ?2 WHERE id = ?1", &[n(a.id), n(beta_credit)]).await?;
                return Ok((self.user(a.id).await?.ok_or("account gone")?, false));
            }
            return Ok((a, false));
        }
        let (plan, topup) = if unlimited { (badi_cloud_core::rules::UNLIMITED, 0) } else if beta { ("beta", beta_credit) } else { ("free", 0) };
        self.run("INSERT INTO users (email, created_at, plan, plan_until, composio_user, topup, name) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6)", &[s(email), n(now), s(plan), s(composio_user), n(topup), name.map(s).unwrap_or(JsValue::NULL)]).await?;
        let u = self.first::<UserRow>(&q, &[s(email)]).await?.ok_or("account not created")?;
        Ok((u.into(), true))
    }

    /// Whether this PC (a hash of its id) already had the free starter.
    pub async fn pc_claimed(&self, hwid: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct X {
            #[allow(dead_code)]
            x: f64,
        }
        Ok(self.first::<X>("SELECT 1 AS x FROM free_claims WHERE hwid = ?1", &[s(hwid)]).await?.is_some())
    }

    /// Gives the free starter once: the PC is claimed first (a race between two sign-ins on one PC gives
    /// it once), then the account. Returns whether it was given.
    pub async fn claim_free(&self, user: i64, hwid: &str, credits: i64, now: i64) -> Result<bool> {
        let r = self.0.prepare("INSERT OR IGNORE INTO free_claims (hwid, user_id, claimed_at) VALUES (?1, ?2, ?3)").bind(&[s(hwid), n(user), n(now)])?.run().await?;
        if r.meta()?.and_then(|m| m.changes).unwrap_or(0) == 0 {
            return Ok(false);
        }
        let r = self.0.prepare("UPDATE users SET free_claimed = 1, topup = topup + ?2 WHERE id = ?1 AND free_claimed = 0").bind(&[n(user), n(credits)])?.run().await?;
        Ok(r.meta()?.and_then(|m| m.changes).unwrap_or(0) > 0)
    }

    pub async fn user_by_token(&self, hash: &str, now: i64) -> Result<Option<Account>> {
        #[derive(Deserialize)]
        struct T {
            user_id: f64,
        }
        let Some(t) = self.first::<T>("SELECT user_id FROM tokens WHERE hash = ?1", &[s(hash)]).await? else { return Ok(None) };
        self.run("UPDATE tokens SET last_used = ?2 WHERE hash = ?1", &[s(hash), n(now)]).await?;
        self.user(t.user_id as i64).await
    }

    pub async fn add_token(&self, hash: &str, user: i64, label: &str, now: i64) -> Result<()> {
        self.run("INSERT INTO tokens (hash, user_id, created_at, last_used, label) VALUES (?1, ?2, ?3, ?3, ?4)", &[s(hash), n(user), n(now), s(label)]).await
    }

    pub async fn drop_token(&self, hash: &str) -> Result<()> {
        self.run("DELETE FROM tokens WHERE hash = ?1", &[s(hash)]).await
    }

    /// Starts a Google sign-in: our state for Google, tied to the app's state, or (`next`, a web login)
    /// to the page to go back to.
    pub async fn put_oauth_state(&self, state: &str, app_state: &str, next: Option<&str>, expires: i64, now: i64) -> Result<()> {
        self.run("DELETE FROM oauth_states WHERE expires < ?1", &[n(now)]).await?;
        let next = next.map(s).unwrap_or(JsValue::NULL);
        self.run("INSERT INTO oauth_states (state, app_state, expires, next) VALUES (?1, ?2, ?3, ?4)", &[s(state), s(app_state), n(expires), next]).await
    }

    /// Takes a sign-in's state (one use); returns (the app's state, the web page to go back to) while it
    /// is fresh.
    pub async fn take_oauth_state(&self, state: &str, now: i64) -> Result<Option<(String, Option<String>)>> {
        #[derive(Deserialize)]
        struct O {
            app_state: String,
            expires: f64,
            next: Option<String>,
        }
        let row = self.first::<O>("SELECT app_state, expires, next FROM oauth_states WHERE state = ?1", &[s(state)]).await?;
        self.run("DELETE FROM oauth_states WHERE state = ?1", &[s(state)]).await?;
        Ok(row.filter(|o| o.expires as i64 > now).map(|o| (o.app_state, o.next)))
    }

    /// An order as the website shows it to the person whose order it is.
    pub async fn payment(&self, order: &str) -> Result<Option<Payment>> {
        #[derive(Deserialize)]
        struct P {
            user_id: f64,
            item: String,
            amount: f64,
            status: String,
        }
        Ok(self
            .first::<P>("SELECT user_id, item, amount, status FROM payments WHERE order_id = ?1", &[s(order)])
            .await?
            .map(|p| Payment { user: p.user_id as i64, item: p.item, amount: p.amount as i64, status: p.status }))
    }

    /// A one-use code that hands a login back to the app that asked for it.
    pub async fn put_grant(&self, hash: &str, user: i64, app_state: &str, expires: i64, now: i64) -> Result<()> {
        self.run("DELETE FROM login_grants WHERE expires < ?1", &[n(now)]).await?;
        self.run("INSERT INTO login_grants (hash, user_id, app_state, expires) VALUES (?1, ?2, ?3, ?4)", &[s(hash), n(user), s(app_state), n(expires)]).await
    }

    /// Takes a grant (one use): its account, when it is fresh and for this app state.
    pub async fn take_grant(&self, hash: &str, app_state: &str, now: i64) -> Result<Option<i64>> {
        #[derive(Deserialize)]
        struct G {
            user_id: f64,
            app_state: String,
            expires: f64,
        }
        let row = self.first::<G>("SELECT user_id, app_state, expires FROM login_grants WHERE hash = ?1", &[s(hash)]).await?;
        self.run("DELETE FROM login_grants WHERE hash = ?1", &[s(hash)]).await?;
        Ok(row.filter(|g| g.expires as i64 > now && badi_cloud_core::same(&g.app_state, app_state)).map(|g| g.user_id as i64))
    }

    /// Books a call's cost (milli-rupiah) as credits at the rate in force: the subscription's first, then
    /// the credits that never lapse. Reads the latest balances first.
    pub async fn spend(&self, user: i64, cost: i64, now: i64) -> Result<()> {
        if cost <= 0 {
            return Ok(());
        }
        let Some(a) = self.user(user).await? else { return Ok(()) };
        if a.unlimited() {
            return Ok(());
        }
        let c = self.catalog().await?;
        let mut m = a.meter;
        m.spend(c.to_credits(cost), a.subscription(&c, now).is_some());
        self.run("UPDATE users SET sub_left = ?2, topup = ?3 WHERE id = ?1", &[n(user), n(m.sub_left), n(m.topup)]).await
    }

    /// A task that began within the limits may finish.
    pub async fn task_continues(&self, user: i64, task: &str, now: i64) -> Result<bool> {
        #[derive(Deserialize)]
        struct T {
            until: f64,
        }
        Ok(self.first::<T>("SELECT until FROM tasks WHERE user_id = ?1 AND task = ?2", &[n(user), s(task)]).await?.is_some_and(|t| t.until as i64 > now))
    }

    pub async fn allow_task(&self, user: i64, task: &str, until: i64, now: i64) -> Result<()> {
        self.run("DELETE FROM tasks WHERE until < ?1", &[n(now)]).await?;
        self.run("INSERT OR REPLACE INTO tasks (user_id, task, until) VALUES (?1, ?2, ?3)", &[n(user), s(task), n(until)]).await
    }

    /// A new order: `kind` is 'checkout' (paid by the person) or 'renewal' (the saved card).
    pub async fn add_payment(&self, order: &str, user: i64, item: &str, amount: i64, kind: &str, now: i64) -> Result<()> {
        self.run("INSERT INTO payments (order_id, user_id, item, amount, status, created_at, kind) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6)", &[s(order), n(user), s(item), n(amount), n(now), s(kind)]).await
    }

    /// Orders still waiting for Midtrans's word, made since `since` (one account's, or everyone's).
    pub async fn pending_payments(&self, user: Option<i64>, since: i64) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct O {
            order_id: String,
        }
        let rows = match user {
            Some(u) => self.0.prepare("SELECT order_id FROM payments WHERE status = 'pending' AND created_at >= ?1 AND user_id = ?2 ORDER BY created_at DESC LIMIT 10").bind(&[n(since), n(u)])?,
            None => self.0.prepare("SELECT order_id FROM payments WHERE status = 'pending' AND created_at >= ?1 ORDER BY created_at DESC LIMIT 100").bind(&[n(since)])?,
        };
        Ok(rows.all().await?.results::<O>()?.into_iter().map(|o| o.order_id).collect())
    }

    /// Accounts whose card subscription may be due (the rules decide), without one charged in the last
    /// `recent` ms, so a cycle is never charged twice.
    pub async fn renewal_candidates(&self, by: i64, recent: i64) -> Result<Vec<Account>> {
        let q = format!(
            "SELECT {USER} FROM users WHERE auto_renew = 1 AND card_token IS NOT NULL AND plan_until <= ?1
             AND NOT EXISTS (SELECT 1 FROM payments p WHERE p.user_id = users.id AND p.kind = 'renewal' AND p.created_at >= ?2)"
        );
        Ok(self.0.prepare(&q).bind(&[n(by), n(recent)])?.all().await?.results::<UserRow>()?.into_iter().map(Into::into).collect())
    }

    /// The saved card's token, for a renewal charge only.
    pub async fn renewal_token(&self, user: i64) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct T {
            card_token: Option<String>,
        }
        Ok(self.first::<T>("SELECT card_token FROM users WHERE id = ?1", &[n(user)]).await?.and_then(|t| t.card_token))
    }

    /// A subscription paid by card: its card renews the next cycles.
    pub async fn save_card(&self, user: i64, token: &str, mask: &str, expires: i64) -> Result<()> {
        self.run("UPDATE users SET card_token = ?2, card_mask = ?3, card_expires = ?4, auto_renew = 1, renew_failures = 0 WHERE id = ?1", &[n(user), s(token), s(mask), n(expires)]).await
    }

    /// Renewing on or off (cancel / resume); a fresh start clears the refused tries.
    pub async fn set_auto_renew(&self, user: i64, on: bool) -> Result<()> {
        self.run("UPDATE users SET auto_renew = ?2, renew_failures = 0 WHERE id = ?1", &[n(user), n(on as i64)]).await
    }

    /// A refused renewal charge: counted, and renewing stops at the last try (the count stays to say why).
    pub async fn renewal_refused(&self, user: i64, failures: i64, stop: bool) -> Result<()> {
        self.run("UPDATE users SET renew_failures = ?2, auto_renew = CASE WHEN ?3 = 1 THEN 0 ELSE auto_renew END WHERE id = ?1", &[n(user), n(failures), n(stop as i64)]).await
    }

    /// Forgets the saved card (the person asked, or it lapsed); renewing stops.
    pub async fn drop_card(&self, user: i64) -> Result<()> {
        self.run("UPDATE users SET card_token = NULL, card_mask = NULL, card_expires = 0, auto_renew = 0 WHERE id = ?1", &[n(user)]).await
    }

    /// Marks a payment paid once. Returns (user, item, amount, kind) the first time, None after.
    pub async fn mark_paid(&self, order: &str, now: i64) -> Result<Option<(i64, String, i64, String)>> {
        #[derive(Deserialize)]
        struct P {
            user_id: f64,
            item: String,
            amount: f64,
            status: String,
            kind: String,
        }
        let Some(p) = self.first::<P>("SELECT user_id, item, amount, status, kind FROM payments WHERE order_id = ?1", &[s(order)]).await? else { return Ok(None) };
        if p.status == "paid" {
            return Ok(None);
        }
        let r = self.0.prepare("UPDATE payments SET status = 'paid', paid_at = ?2 WHERE order_id = ?1 AND status != 'paid'").bind(&[s(order), n(now)])?.run().await?;
        // A second notification that raced this one changes nothing.
        if r.meta()?.and_then(|m| m.changes).unwrap_or(1) == 0 {
            return Ok(None);
        }
        Ok(Some((p.user_id as i64, p.item, p.amount as i64, p.kind)))
    }

    pub async fn set_payment_status(&self, order: &str, status: &str) -> Result<()> {
        self.run("UPDATE payments SET status = ?2 WHERE order_id = ?1 AND status != 'paid'", &[s(order), s(status)]).await
    }

    /// A paid subscription: a new cycle with its credits (any refused renewal tries are over).
    pub async fn start_subscription(&self, user: i64, plan_id: &str, until: i64, sub_left: i64) -> Result<()> {
        self.run("UPDATE users SET plan = ?2, plan_until = ?3, sub_left = ?4, renew_failures = 0 WHERE id = ?1", &[n(user), s(plan_id), n(until), n(sub_left)]).await
    }

    // ---- the closed beta's invite list (the admin API) ----

    pub async fn invited(&self, email: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct One {
            #[allow(dead_code)]
            email: String,
        }
        Ok(self.first::<One>("SELECT email FROM beta_invites WHERE email = ?1", &[s(email)]).await?.is_some())
    }

    /// Adds invites; returns whether each one is new.
    pub async fn invite(&self, email: &str, note: &str, now: i64) -> Result<bool> {
        if self.invited(email).await? {
            return Ok(false);
        }
        self.run("INSERT INTO beta_invites (email, note, added_at) VALUES (?1, ?2, ?3)", &[s(email), s(note), n(now)]).await?;
        Ok(true)
    }

    /// Takes an invite back; a beta account under it goes back to Free without its beta credit (its other
    /// data stays).
    pub async fn uninvite(&self, email: &str) -> Result<bool> {
        let had = self.invited(email).await?;
        self.run("DELETE FROM beta_invites WHERE email = ?1", &[s(email)]).await?;
        self.run("UPDATE users SET plan = 'free', topup = 0 WHERE email = ?1 AND plan = 'beta'", &[s(email)]).await?;
        Ok(had)
    }

    /// The invites, newest first, with the account each one made (if any).
    pub async fn invites(&self) -> Result<Vec<serde_json::Value>> {
        #[derive(Deserialize)]
        struct Row {
            email: String,
            note: String,
            added_at: f64,
            plan: Option<String>,
            plan_until: Option<f64>,
            topup: Option<f64>,
            created_at: Option<f64>,
        }
        let rows = self
            .0
            .prepare("SELECT b.email, b.note, b.added_at, u.plan, u.plan_until, u.topup, u.created_at FROM beta_invites b LEFT JOIN users u ON u.email = b.email ORDER BY b.added_at DESC")
            .all()
            .await?
            .results::<Row>()?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let account = r.plan.map(|p| serde_json::json!({ "plan": p, "planUntil": r.plan_until.unwrap_or(0.0) as i64, "creditsLeft": (r.topup.unwrap_or(0.0) / MILLI as f64).floor() as i64, "createdAt": r.created_at.unwrap_or(0.0) as i64 }));
                serde_json::json!({ "email": r.email, "note": r.note, "addedAt": r.added_at as i64, "account": account })
            })
            .collect())
    }

    /// An account by email (the admin API).
    pub async fn user_by_email(&self, email: &str) -> Result<Option<Account>> {
        Ok(self.first::<UserRow>(&format!("SELECT {USER} FROM users WHERE email = ?1"), &[s(email)]).await?.map(Into::into))
    }

    /// Whether this account ever completed a payment (a subscription or a top-up).
    pub async fn has_paid(&self, user: i64) -> Result<bool> {
        #[derive(Deserialize)]
        struct T {
            #[allow(dead_code)]
            order_id: String,
        }
        Ok(self.first::<T>("SELECT order_id FROM payments WHERE user_id = ?1 AND status = 'paid' LIMIT 1", &[n(user)]).await?.is_some())
    }

    /// Adds milli-credits that never lapse (a top-up, the free starter, an admin grant).
    pub async fn add_topup(&self, user: i64, credit: i64) -> Result<()> {
        self.run("UPDATE users SET topup = topup + ?2 WHERE id = ?1", &[n(user), n(credit)]).await
    }

    pub async fn own_session(&self, session: &str, user: i64) -> Result<()> {
        self.run("INSERT OR REPLACE INTO composio_sessions (session_id, user_id) VALUES (?1, ?2)", &[s(session), n(user)]).await
    }

    pub async fn owns_session(&self, session: &str, user: i64) -> Result<bool> {
        #[derive(Deserialize)]
        struct X {
            #[allow(dead_code)]
            x: f64,
        }
        Ok(self.first::<X>("SELECT 1 AS x FROM composio_sessions WHERE session_id = ?1 AND user_id = ?2", &[s(session), n(user)]).await?.is_some())
    }

    /// The right to erasure (UU PDP). Payments stay for the tax records, without the account.
    pub async fn delete_user(&self, user: i64) -> Result<()> {
        for sql in ["DELETE FROM tokens WHERE user_id = ?1", "DELETE FROM composio_sessions WHERE user_id = ?1", "DELETE FROM tasks WHERE user_id = ?1", "DELETE FROM login_grants WHERE user_id = ?1", "DELETE FROM users WHERE id = ?1"] {
            self.run(sql, &[n(user)]).await?;
        }
        Ok(())
    }
}
