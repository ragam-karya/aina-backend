-- Card subscriptions renew by themselves (2026-10-20). Midtrans keeps the card; Badi keeps only its
-- token (to charge the next cycle), the masked number to show, and when the token lapses.
ALTER TABLE users ADD COLUMN auto_renew INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN card_token TEXT;
ALTER TABLE users ADD COLUMN card_mask TEXT;
ALTER TABLE users ADD COLUMN card_expires INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN renew_failures INTEGER NOT NULL DEFAULT 0;
-- 'checkout' (paid by the person) or 'renewal' (the saved card, charged by Badi).
ALTER TABLE payments ADD COLUMN kind TEXT NOT NULL DEFAULT 'checkout';
