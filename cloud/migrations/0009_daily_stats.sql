-- The daily rollup of the analytics (2026-11-10, docs/analytics.md): yesterday's numbers from Workers
-- Analytics Engine (which keeps three months), kept here for good. One row per WIB day, metric and
-- dimension ("" for a plain total; "messages:claude-sonnet-5-5", "linkedin", … otherwise). A day is done
-- once it has the row (day, 'rolled', '').
CREATE TABLE IF NOT EXISTS daily_stats (
  day TEXT NOT NULL,
  metric TEXT NOT NULL,
  dim TEXT NOT NULL DEFAULT '',
  value REAL NOT NULL,
  PRIMARY KEY (day, metric, dim)
);
