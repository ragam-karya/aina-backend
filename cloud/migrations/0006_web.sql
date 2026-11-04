-- Subscribing from the browser (2026-10-20): a Google login started on the web goes back to a page of
-- Badi Cloud (`next`) with a session cookie, instead of handing a code to the app. NULL: the app's login.
ALTER TABLE oauth_states ADD COLUMN next TEXT;
