-- Exact time-range matching for recurring objects.
--
-- 1 if range_start/range_end only span the hull of several instances
-- (RRULE, RDATE, overrides): calendar-query then checks each candidate's
-- instances exactly. 0 for single instances, whose range is exact (or
-- conservatively widened) already.
ALTER TABLE items ADD COLUMN recurring INTEGER NOT NULL DEFAULT 0;

-- Version of the indexing that computed range_start, range_end and
-- recurring. Rows of older versions keep their (conservative, never too
-- narrow) ranges until the server re-indexes them in the background.
ALTER TABLE items ADD COLUMN index_version INTEGER NOT NULL DEFAULT 0;

CREATE INDEX items_by_index_version ON items (index_version, id);
