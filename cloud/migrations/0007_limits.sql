-- Hard limits (2026-11-04). Credits spent past zero are now owed (topup goes negative) instead of
-- forgiven; a restricted account (a chargeback, or the admin API) is refused every paid call.
ALTER TABLE users ADD COLUMN suspended TEXT; -- NULL: fine; otherwise why it is restricted
-- Paid calls running per account, so parallel calls can't all start on the last credit.
CREATE TABLE IF NOT EXISTS inflight (id TEXT PRIMARY KEY, user_id INTEGER NOT NULL, until INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS inflight_user ON inflight (user_id, until);
-- Restricted emails (a SHA-256 of the address) outlive a deleted account, so deleting and signing in
-- again does not lift the restriction.
CREATE TABLE IF NOT EXISTS blocked_emails (email_hash TEXT PRIMARY KEY, reason TEXT NOT NULL, at INTEGER NOT NULL);
