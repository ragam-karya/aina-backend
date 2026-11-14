-- aina-usage: AI usage for analytics and the business (docs/usage-analytics.md). Its own D1 database
-- (binding USAGE), apart from accounts and billing (badi-cloud), so it can be queried, exported or moved
-- on its own. Plain SQLite: no JSON, no triggers. Never the prompt, screenshot, audio or answer; users
-- are their id in badi-cloud's users table.
--
-- Units: money is kept in thousandths so nothing is lost by rounding (cost_milli_idr / 1000 = rupiah,
-- credits_milli / 1000 = aina credits). Days and hours are WIB.

-- One row per AI call, whatever came of it. Kept KEEP_CALLS_DAYS (180) days, then only the daily totals.
CREATE TABLE IF NOT EXISTS ai_calls (
  id INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,                 -- when it started, ms since 1970 (UTC)
  day TEXT NOT NULL,                   -- '2026-11-14' (WIB)
  hour INTEGER NOT NULL,               -- 0-23 (WIB)
  user_id INTEGER NOT NULL,
  provider TEXT NOT NULL,              -- anthropic | openai | elevenlabs
  route TEXT NOT NULL,                 -- messages, transcribe, speech, eleven, voice_line, realtime, realtime_session
  model TEXT NOT NULL,
  feature TEXT NOT NULL DEFAULT '',    -- what in the app asked (x-aina-kind): tanya, agent, dikte, ...
  app_version TEXT NOT NULL DEFAULT '',
  outcome TEXT NOT NULL,               -- ok | refused (aina Cloud's rules) | upstream (provider error) | error (unreachable)
  status INTEGER NOT NULL DEFAULT 0,   -- HTTP status
  reason TEXT NOT NULL DEFAULT '',     -- why it was refused or failed
  stream INTEGER NOT NULL DEFAULT 0,
  input_tokens INTEGER NOT NULL DEFAULT 0,       -- text/images in, not from cache
  output_tokens INTEGER NOT NULL DEFAULT 0,      -- text out
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0,
  audio_in_tokens INTEGER NOT NULL DEFAULT 0,    -- realtime
  audio_out_tokens INTEGER NOT NULL DEFAULT 0,   -- realtime
  web_searches INTEGER NOT NULL DEFAULT 0,
  audio_ms INTEGER NOT NULL DEFAULT 0,           -- transcribed audio
  chars INTEGER NOT NULL DEFAULT 0,              -- text read out (speech, eleven, voice_line)
  cost_milli_idr INTEGER NOT NULL DEFAULT 0,     -- what aina pays the provider (HPP), even when the user pays nothing
  credits_milli INTEGER NOT NULL DEFAULT 0,      -- what was taken from the account's credits
  first_ms INTEGER NOT NULL DEFAULT 0,           -- to the provider's first byte; 0 = unknown
  total_ms INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS ai_calls_day ON ai_calls (day);
CREATE INDEX IF NOT EXISTS ai_calls_user ON ai_calls (user_id, day);

-- The totals per WIB day, user, provider, model, route and feature: kept for good.
CREATE TABLE IF NOT EXISTS ai_usage_daily (
  day TEXT NOT NULL,
  user_id INTEGER NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  route TEXT NOT NULL,
  feature TEXT NOT NULL DEFAULT '',
  calls_ok INTEGER NOT NULL DEFAULT 0,
  calls_failed INTEGER NOT NULL DEFAULT 0,       -- refused, upstream and error
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0,
  audio_in_tokens INTEGER NOT NULL DEFAULT 0,
  audio_out_tokens INTEGER NOT NULL DEFAULT 0,
  web_searches INTEGER NOT NULL DEFAULT 0,
  audio_ms INTEGER NOT NULL DEFAULT 0,
  chars INTEGER NOT NULL DEFAULT 0,
  cost_milli_idr INTEGER NOT NULL DEFAULT 0,
  credits_milli INTEGER NOT NULL DEFAULT 0,
  total_ms INTEGER NOT NULL DEFAULT 0,           -- summed over the ok calls (mean = total_ms / calls_ok)
  PRIMARY KEY (day, user_id, provider, model, route, feature)
);
CREATE INDEX IF NOT EXISTS ai_usage_user ON ai_usage_daily (user_id, day);

-- People who chose to be on the leaderboard (GET/PUT/DELETE /v1/leaderboard), with the name shown there.
CREATE TABLE IF NOT EXISTS leaderboard (
  user_id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  joined_at INTEGER NOT NULL
);

-- ---------- views: the questions asked most, ready to read ----------

-- Per day: how many people used the AI, how often aina helped (an answer from Claude or a realtime
-- reply), the tokens, the real cost and the credits.
CREATE VIEW IF NOT EXISTS v_daily AS
SELECT day,
  COUNT(DISTINCT CASE WHEN calls_ok > 0 THEN user_id END) AS active_users,
  SUM(CASE WHEN route IN ('messages', 'realtime') THEN calls_ok ELSE 0 END) AS helps,
  SUM(calls_ok) AS calls_ok,
  SUM(calls_failed) AS calls_failed,
  SUM(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens + audio_in_tokens + audio_out_tokens) AS tokens,
  SUM(cost_milli_idr) / 1000.0 AS cost_idr,
  SUM(credits_milli) / 1000.0 AS credits
FROM ai_usage_daily GROUP BY day;

-- Per month, provider and model: what each costs and how well it does (evidence for changing providers).
CREATE VIEW IF NOT EXISTS v_models_monthly AS
SELECT substr(day, 1, 7) AS month, provider, model,
  SUM(calls_ok) AS calls_ok,
  SUM(calls_failed) AS calls_failed,
  SUM(input_tokens) AS input_tokens, SUM(output_tokens) AS output_tokens,
  SUM(cache_read_tokens) AS cache_read_tokens, SUM(cache_write_tokens) AS cache_write_tokens,
  SUM(audio_in_tokens) AS audio_in_tokens, SUM(audio_out_tokens) AS audio_out_tokens,
  SUM(chars) AS chars, SUM(audio_ms) / 60000.0 AS audio_minutes,
  SUM(cost_milli_idr) / 1000.0 AS cost_idr,
  SUM(credits_milli) / 1000.0 AS credits,
  CASE WHEN SUM(calls_ok) > 0 THEN SUM(total_ms) / SUM(calls_ok) END AS avg_ms
FROM ai_usage_daily GROUP BY month, provider, model;

-- Per person, all time: since when, how many days, how often aina helped, tokens, real cost, credits.
CREATE VIEW IF NOT EXISTS v_users AS
SELECT user_id,
  MIN(day) AS first_day, MAX(day) AS last_day,
  COUNT(DISTINCT CASE WHEN calls_ok > 0 THEN day END) AS active_days,
  SUM(CASE WHEN route IN ('messages', 'realtime') THEN calls_ok ELSE 0 END) AS helps,
  SUM(calls_ok) AS calls_ok,
  SUM(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens + audio_in_tokens + audio_out_tokens) AS tokens,
  SUM(cost_milli_idr) / 1000.0 AS cost_idr,
  SUM(credits_milli) / 1000.0 AS credits
FROM ai_usage_daily GROUP BY user_id;

-- Per month and app feature: how many people use it and how often.
CREATE VIEW IF NOT EXISTS v_features_monthly AS
SELECT substr(day, 1, 7) AS month, feature,
  COUNT(DISTINCT user_id) AS users,
  SUM(calls_ok) AS calls_ok,
  SUM(cost_milli_idr) / 1000.0 AS cost_idr
FROM ai_usage_daily GROUP BY month, feature;

-- When in the day people use aina (WIB hour), over the calls still kept one by one.
CREATE VIEW IF NOT EXISTS v_hours AS
SELECT hour, COUNT(*) AS calls, COUNT(DISTINCT user_id) AS users
FROM ai_calls WHERE outcome = 'ok' GROUP BY hour;
