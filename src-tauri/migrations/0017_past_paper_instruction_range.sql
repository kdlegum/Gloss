-- Per-document instruction page range for past-paper chunking.
-- NULL/NULL means "no instruction pages".

ALTER TABLE source_documents
    ADD COLUMN instruction_page_start INTEGER;

ALTER TABLE source_documents
    ADD COLUMN instruction_page_end INTEGER
    CHECK (
        (instruction_page_start IS NULL AND instruction_page_end IS NULL) OR
        (
            instruction_page_start IS NOT NULL AND
            instruction_page_end IS NOT NULL AND
            instruction_page_start >= 1 AND
            instruction_page_end >= 1 AND
            instruction_page_start <= instruction_page_end
        )
    );

-- Backfill existing past-paper documents to the default instruction range (page 1 only).
UPDATE source_documents
SET instruction_page_start = 1,
    instruction_page_end = 1
WHERE lower(document_mode) = 'past_paper'
  AND instruction_page_start IS NULL
  AND instruction_page_end IS NULL;
