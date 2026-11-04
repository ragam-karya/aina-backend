-- Credits (price list of 2026-10). The plans live here so prices, quotas and badges change with an
-- UPDATE (or the admin API: PUT /admin/plans) instead of a release. Balances are milli-credits.
CREATE TABLE IF NOT EXISTS plans (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('free', 'subscription', 'topup', 'enterprise', 'beta')),
  price INTEGER NOT NULL DEFAULT 0,   -- rupiah, per cycle or per pack; 0 for free and custom
  credits INTEGER NOT NULL DEFAULT 0, -- per cycle or per pack
  days INTEGER NOT NULL DEFAULT 0,    -- a subscription's cycle
  badge TEXT NOT NULL DEFAULT '',     -- shown on the plan
  note TEXT NOT NULL DEFAULT '',      -- for the team only, never shown
  sort INTEGER NOT NULL DEFAULT 0,
  active INTEGER NOT NULL DEFAULT 1,  -- 0: not offered (accounts on it keep it until the cycle ends)
  updated_at INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO plans (id, name, kind, price, credits, days, badge, note, sort, active) VALUES
  ('free', 'Free', 'free', 0, 10, 0, '', '1x per HWID, subsidi marketing', 0, 1),
  ('basic', 'Basic', 'subscription', 49000, 85, 30, '', '', 10, 1),
  ('pro', 'Pro', 'subscription', 129000, 225, 30, 'Best Value', '', 20, 1),
  ('topup_small', 'Top-Up Small', 'topup', 49000, 65, 0, '', '', 30, 1),
  ('topup_medium', 'Top-Up Medium', 'topup', 149000, 210, 0, '', 'Decoy', 40, 1),
  ('topup_large', 'Top-Up Large', 'topup', 199000, 300, 0, 'Paling hemat', 'Target', 50, 1),
  ('enterprise', 'Enterprise', 'enterprise', 0, 0, 0, '', 'Custom seat & kuota', 60, 1),
  ('beta', 'Beta', 'beta', 0, 318, 0, '', 'Beta tertutup', 90, 0);

-- rupiah_per_credit: API cost (HPP) one credit stands for. enterprise_contact: a mailto: or https: link.
CREATE TABLE IF NOT EXISTS billing_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT OR IGNORE INTO billing_settings (key, value) VALUES ('rupiah_per_credit', '170'), ('enterprise_contact', '');

-- The free starter, once per PC: a hash of the PC's id (never the id itself).
CREATE TABLE IF NOT EXISTS free_claims (hwid TEXT PRIMARY KEY, user_id INTEGER NOT NULL, claimed_at INTEGER NOT NULL);

-- sub_left: what is left of the current subscription cycle; topup (existing): credits that never lapse.
ALTER TABLE users ADD COLUMN sub_left INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN free_claimed INTEGER NOT NULL DEFAULT 0;

-- Accounts from before credits. Top-up and beta balances were milli-rupiah of API cost: at Rp 170 a
-- credit that is the same allowance in milli-credits. Trials become Free (the starter comes at the next
-- sign-in from a PC that hasn't had it); paid months run on as the nearest new plan, with its credits.
UPDATE users SET topup = topup / 170;
UPDATE users SET plan = 'free', plan_until = 0 WHERE plan = 'trial';
UPDATE users SET plan = 'basic', sub_left = 85000 WHERE plan IN ('lite', 'plus');
UPDATE users SET plan = 'pro', sub_left = 225000 WHERE plan = 'max';
UPDATE users SET sub_left = 225000 WHERE plan = 'pro' AND sub_left = 0;
-- The session and week columns are no longer used.
