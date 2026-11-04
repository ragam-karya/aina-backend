-- Badi Cloud: only what accounts and billing need. No prompts, screenshots, audio or answers.
CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  plan TEXT NOT NULL,
  plan_until INTEGER NOT NULL,
  composio_user TEXT NOT NULL,
  session_start INTEGER NOT NULL DEFAULT 0,
  session_used INTEGER NOT NULL DEFAULT 0,
  week_start INTEGER NOT NULL DEFAULT 0,
  week_used INTEGER NOT NULL DEFAULT 0,
  topup INTEGER NOT NULL DEFAULT 0
);
-- A Google sign-in under way (state -> the app's state), and one-use codes that hand a login back to the app.
CREATE TABLE IF NOT EXISTS oauth_states (state TEXT PRIMARY KEY, app_state TEXT NOT NULL, expires INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS login_grants (hash TEXT PRIMARY KEY, user_id INTEGER NOT NULL, app_state TEXT NOT NULL, expires INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS tokens (hash TEXT PRIMARY KEY, user_id INTEGER NOT NULL, created_at INTEGER NOT NULL, last_used INTEGER NOT NULL, label TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS tokens_user ON tokens (user_id);
CREATE TABLE IF NOT EXISTS payments (order_id TEXT PRIMARY KEY, user_id INTEGER NOT NULL, item TEXT NOT NULL, amount INTEGER NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL, paid_at INTEGER);
CREATE TABLE IF NOT EXISTS composio_sessions (session_id TEXT PRIMARY KEY, user_id INTEGER NOT NULL);
-- Tasks allowed to finish past a limit.
CREATE TABLE IF NOT EXISTS tasks (user_id INTEGER NOT NULL, task TEXT NOT NULL, until INTEGER NOT NULL, PRIMARY KEY (user_id, task));
