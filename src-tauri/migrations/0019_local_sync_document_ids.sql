ALTER TABLE source_documents
    ADD COLUMN document_sync_id TEXT;

ALTER TABLE source_documents
    ADD COLUMN original_file_name TEXT;

CREATE UNIQUE INDEX idx_source_documents_sync_id
    ON source_documents(document_sync_id)
    WHERE document_sync_id IS NOT NULL;
