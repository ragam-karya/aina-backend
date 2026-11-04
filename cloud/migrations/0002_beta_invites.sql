-- The closed beta's invite list (managed through the admin API). Only emails, a note, and when.
CREATE TABLE IF NOT EXISTS beta_invites (email TEXT PRIMARY KEY, note TEXT NOT NULL DEFAULT '', added_at INTEGER NOT NULL);
