CREATE TABLE text_blocks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id     INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    order_idx   INTEGER NOT NULL,
    bbox_x      REAL    NOT NULL,
    bbox_y      REAL    NOT NULL,
    bbox_w      REAL    NOT NULL,
    bbox_h      REAL    NOT NULL,
    text        TEXT    NOT NULL,
    chunk_id    INTEGER REFERENCES chunks(id) ON DELETE SET NULL
);

CREATE INDEX idx_text_blocks_page  ON text_blocks(page_id);
CREATE INDEX idx_text_blocks_chunk ON text_blocks(chunk_id);

ALTER TABLE source_documents ADD COLUMN chunking_status TEXT NOT NULL DEFAULT 'pending'
    CHECK (chunking_status IN ('pending', 'extracting', 'grouping', 'done', 'failed'));
