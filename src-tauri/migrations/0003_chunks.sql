CREATE TABLE chunks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    source_document_id  INTEGER NOT NULL REFERENCES source_documents(id) ON DELETE CASCADE,
    page_id      INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    chunk_type   TEXT    NOT NULL CHECK(chunk_type IN ('definition', 'theorem', 'proof', 'exercise', 'example', 'other')),
    bbox_x       REAL    NOT NULL,
    bbox_y       REAL    NOT NULL,
    bbox_w       REAL    NOT NULL,
    bbox_h       REAL    NOT NULL,
    ocr_text     TEXT,
    ai_tags      TEXT,
    status       TEXT    NOT NULL DEFAULT 'incomplete' CHECK(status IN ('incomplete', 'in_progress', 'complete')),
    ai_suggested INTEGER NOT NULL DEFAULT 0,
    created_at   DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_chunks_page     ON chunks(page_id);
CREATE INDEX idx_chunks_source_document ON chunks(source_document_id);

ALTER TABLE strokes ADD COLUMN chunk_id INTEGER REFERENCES chunks(id) ON DELETE SET NULL;
