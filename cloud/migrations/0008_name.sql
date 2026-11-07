-- The person's name from Google (2026-11-07), for the website's greeting and account menu. NULL until the
-- first sign-in that asks Google for the profile scope.
ALTER TABLE users ADD COLUMN name TEXT;
