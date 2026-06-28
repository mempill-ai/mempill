-- migrations/V3__add_date_granularity.sql
-- mempill-postgres v3 migration: add per-endpoint date-granularity columns to claims.
--
-- These columns persist the display-only precision hint that records how a partial
-- valid-time date string (e.g. "2024", "2024-05") was specified by the host.
-- They are DISPLAY-ONLY: no matching or fold logic reads them.
--
-- Nullable TEXT so existing rows upgrade cleanly: old rows → NULL → None on read.
-- Values are the snake_case strings from DateGranularity serde: "year", "month", "day", "instant".
ALTER TABLE claims ADD COLUMN IF NOT EXISTS valid_time_start_granularity TEXT;
ALTER TABLE claims ADD COLUMN IF NOT EXISTS valid_time_end_granularity TEXT;
