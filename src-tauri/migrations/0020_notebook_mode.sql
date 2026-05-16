-- Allow source_documents.document_mode = 'notebook'.
--
-- Rebuilding source_documents would trip existing child-table foreign keys
-- on installed databases. This migration only broadens the existing CHECK
-- constraint, so update the schema definition in place and bump the schema
-- cookie so SQLite reparses it on this connection.

DROP TABLE IF EXISTS source_documents_new;

PRAGMA writable_schema = ON;

UPDATE sqlite_schema
SET sql = replace(
    sql,
    'CHECK (document_mode IN (''textbook'', ''past_paper''))',
    'CHECK (document_mode IN (''textbook'', ''past_paper'', ''notebook''))'
)
WHERE type = 'table'
  AND name = 'source_documents'
  AND sql LIKE '%CHECK (document_mode IN (''textbook'', ''past_paper''))%';

PRAGMA writable_schema = OFF;
PRAGMA schema_version = 2026050801;
