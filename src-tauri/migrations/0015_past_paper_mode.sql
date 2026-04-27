-- Past paper mode support.
-- Adds document mode, question chunk metadata, cross-page source slices,
-- and achieved-marks history.

PRAGMA foreign_keys = OFF;

ALTER TABLE source_documents
    ADD COLUMN document_mode TEXT NOT NULL DEFAULT 'textbook'
    CHECK (document_mode IN ('textbook', 'past_paper'));

CREATE TABLE chunks_new (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    source_document_id       INTEGER NOT NULL REFERENCES source_documents(id) ON DELETE CASCADE,
    page_id                  INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    chunk_type               TEXT    NOT NULL CHECK(chunk_type IN (
        'definition', 'theorem', 'proof', 'exercise', 'example', 'explanation', 'noise', 'question'
    )),
    bbox_x                   REAL    NOT NULL,
    bbox_y                   REAL    NOT NULL,
    bbox_w                   REAL    NOT NULL,
    bbox_h                   REAL    NOT NULL,
    ocr_text                 TEXT,
    ai_tags                  TEXT,
    status                   TEXT    NOT NULL DEFAULT 'incomplete' CHECK(status IN ('incomplete', 'in_progress', 'complete')),
    ai_suggested             INTEGER NOT NULL DEFAULT 0,
    created_at               DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    title                    TEXT,
    subject                  TEXT,
    proves_chunk_id          INTEGER REFERENCES chunks_new(id) ON DELETE SET NULL,
    formatted_body_md        TEXT,
    glossary_md              TEXT,
    question_label           TEXT,
    available_marks          INTEGER,
    achieved_marks           REAL,
    achieved_marks_updated_at DATETIME,
    CHECK (available_marks IS NULL OR available_marks >= 0),
    CHECK (achieved_marks IS NULL OR achieved_marks >= 0),
    CHECK (available_marks IS NULL OR achieved_marks IS NULL OR achieved_marks <= available_marks)
);

INSERT INTO chunks_new (
    id, source_document_id, page_id, chunk_type, bbox_x, bbox_y, bbox_w, bbox_h,
    ocr_text, ai_tags, status, ai_suggested, created_at,
    title, subject, proves_chunk_id, formatted_body_md, glossary_md
)
SELECT
    id, source_document_id, page_id, chunk_type, bbox_x, bbox_y, bbox_w, bbox_h,
    ocr_text, ai_tags, status, ai_suggested, created_at,
    title, subject, proves_chunk_id, formatted_body_md, glossary_md
FROM chunks;

DROP TABLE chunks;
ALTER TABLE chunks_new RENAME TO chunks;

CREATE INDEX idx_chunks_page ON chunks(page_id);
CREATE INDEX idx_chunks_source_document ON chunks(source_document_id);

CREATE TABLE question_page_slices (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    chunk_id   INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    page_id    INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    bbox_x     REAL    NOT NULL,
    bbox_y     REAL    NOT NULL,
    bbox_w     REAL    NOT NULL,
    bbox_h     REAL    NOT NULL,
    slice_order INTEGER NOT NULL DEFAULT 0,
    UNIQUE (chunk_id, page_id)
);

CREATE INDEX idx_question_slices_page ON question_page_slices(page_id);
CREATE INDEX idx_question_slices_chunk ON question_page_slices(chunk_id);

CREATE TABLE question_mark_attempts (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    chunk_id                INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    source                  TEXT    NOT NULL CHECK(source IN ('ai', 'manual')),
    achieved_marks          REAL,
    available_marks_snapshot INTEGER,
    feedback_md             TEXT,
    include_visuals         INTEGER NOT NULL DEFAULT 0,
    provider                TEXT,
    model                   TEXT,
    created_at              DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_question_mark_attempts_chunk ON question_mark_attempts(chunk_id);
CREATE INDEX idx_question_mark_attempts_created_at ON question_mark_attempts(created_at);

PRAGMA foreign_keys = ON;
