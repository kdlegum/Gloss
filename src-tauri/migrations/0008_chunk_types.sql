-- Rename chunk_type 'other' -> 'explanation', add 'noise'.
-- SQLite can't ALTER a CHECK constraint, so we recreate the table.

PRAGMA foreign_keys = OFF;

CREATE TABLE chunks_new (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    source_document_id INTEGER NOT NULL REFERENCES source_documents(id) ON DELETE CASCADE,
    page_id            INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    chunk_type         TEXT    NOT NULL CHECK(chunk_type IN ('definition', 'theorem', 'proof', 'exercise', 'example', 'explanation', 'noise')),
    bbox_x             REAL    NOT NULL,
    bbox_y             REAL    NOT NULL,
    bbox_w             REAL    NOT NULL,
    bbox_h             REAL    NOT NULL,
    ocr_text           TEXT,
    ai_tags            TEXT,
    status             TEXT    NOT NULL DEFAULT 'incomplete' CHECK(status IN ('incomplete', 'in_progress', 'complete')),
    ai_suggested       INTEGER NOT NULL DEFAULT 0,
    created_at         DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT INTO chunks_new
    SELECT id, source_document_id, page_id,
           CASE chunk_type WHEN 'other' THEN 'explanation' ELSE chunk_type END,
           bbox_x, bbox_y, bbox_w, bbox_h,
           ocr_text, ai_tags, status, ai_suggested, created_at
    FROM chunks;

DROP TABLE chunks;
ALTER TABLE chunks_new RENAME TO chunks;

CREATE INDEX idx_chunks_page            ON chunks(page_id);
CREATE INDEX idx_chunks_source_document ON chunks(source_document_id);

PRAGMA foreign_keys = ON;
