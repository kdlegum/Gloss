CREATE TABLE source_documents (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    title      TEXT    NOT NULL,
    file_path  TEXT    NOT NULL,
    file_hash  TEXT,
    page_count INTEGER
);

CREATE TABLE pages (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    source_document_id  INTEGER NOT NULL REFERENCES source_documents(id) ON DELETE CASCADE,
    page_number         INTEGER NOT NULL, -- One indexed
    UNIQUE (source_document_id, page_number)
);
