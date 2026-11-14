-- The daily rollup's "ai_credits" always held rupiah of API cost (Analytics Engine double8 is milli-rupiah,
-- 2026-11-14): renamed to ai_cost_idr. From now on ai_credits is the credits taken (double11).
UPDATE daily_stats SET metric = 'ai_cost_idr' WHERE metric = 'ai_credits';
