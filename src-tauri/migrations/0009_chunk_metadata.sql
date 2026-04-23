-- Add semantic metadata fields to chunks.
-- All three columns are nullable so existing rows stay valid.
-- SQLite allows ALTER TABLE ADD COLUMN for nullable FK columns,
-- so no table-rebuild is needed here.

ALTER TABLE chunks ADD COLUMN title           TEXT;
ALTER TABLE chunks ADD COLUMN subject         TEXT;
ALTER TABLE chunks ADD COLUMN proves_chunk_id INTEGER REFERENCES chunks(id) ON DELETE SET NULL;
