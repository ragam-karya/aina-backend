-- Production (2026-10-20): the closed beta is over. Beta accounts become Free and keep the beta credits
-- they have left (held as top-up, which never lapses), so they can subscribe or buy Top-Ups like anyone.
UPDATE users SET plan = 'free', plan_until = 0 WHERE plan = 'beta';
-- The owner uses Badi without limits (UNLIMITED_EMAILS in wrangler.toml does the same at each sign-in).
UPDATE users SET plan = 'owner', plan_until = 0 WHERE lower(email) = 'faisalmashuri16@gmail.com';
