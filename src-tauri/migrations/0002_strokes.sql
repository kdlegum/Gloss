CREATE TABLE strokes (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id INTEGER NOT NULL REFERENCES pages(id),
    data    BLOB    NOT NULL,
    colour  TEXT    NOT NULL,
    min_x   REAL    NOT NULL,
    min_y   REAL    NOT NULL,
    max_x   REAL    NOT NULL,
    max_y   REAL    NOT NULL
);

CREATE INDEX idx_strokes_page ON strokes (page_id);
