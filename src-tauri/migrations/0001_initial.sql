CREATE TABLE textbooks (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT    NOT NULL,
    file_path  TEXT    NOT NULL,
    file_hash  TEXT,
    page_count INTEGER
);

CREATE TABLE pages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    textbook_id INTEGER NOT NULL REFERENCES textbooks(id) ON DELETE CASCADE,
    page_number INTEGER NOT NULL, -- One indexed
    UNIQUE (textbook_id, page_number)
);
