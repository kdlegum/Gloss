ALTER TABLE chunks
    ADD COLUMN glossary_format TEXT NOT NULL DEFAULT 'markdown'
    CHECK (glossary_format IN ('markdown', 'typst'));
