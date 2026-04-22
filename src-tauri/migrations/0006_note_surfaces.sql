CREATE TABLE note_surfaces (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id    INTEGER REFERENCES pages(id) ON DELETE CASCADE,
    chunk_id   INTEGER REFERENCES chunks(id) ON DELETE CASCADE,
    kind       TEXT    NOT NULL CHECK(kind IN ('page_notes', 'chunk_notes')),
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK (
        (kind = 'page_notes' AND page_id IS NOT NULL AND chunk_id IS NULL) OR
        (kind = 'chunk_notes' AND chunk_id IS NOT NULL AND page_id IS NULL)
    )
);

CREATE UNIQUE INDEX idx_note_surfaces_page_kind
    ON note_surfaces(page_id, kind)
    WHERE page_id IS NOT NULL;

CREATE UNIQUE INDEX idx_note_surfaces_chunk_kind
    ON note_surfaces(chunk_id, kind)
    WHERE chunk_id IS NOT NULL;

CREATE TABLE surface_strokes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    surface_id INTEGER NOT NULL REFERENCES note_surfaces(id) ON DELETE CASCADE,
    data       BLOB    NOT NULL,
    colour     TEXT    NOT NULL,
    thickness  REAL    NOT NULL DEFAULT 1.0,
    min_x      REAL    NOT NULL,
    min_y      REAL    NOT NULL,
    max_x      REAL    NOT NULL,
    max_y      REAL    NOT NULL,
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_surface_strokes_surface ON surface_strokes(surface_id);
