-- aina's fixed spoken lines are free (2026-11-14, POST /v1/voice/line): the greeting, the replies when
-- called, the filler and the out-of-credits line. The app keeps each clip on disk, so a day needs only
-- a few; this counts them per account and WIB day, so the free route can't be spammed past
-- voice_lines::DAILY_LIMIT. Old days can be deleted any time.
CREATE TABLE IF NOT EXISTS voice_lines (user_id INTEGER NOT NULL, day TEXT NOT NULL, count INTEGER NOT NULL, PRIMARY KEY (user_id, day));
