CREATE TABLE chunk_aliases (
  chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  alias_kind TEXT NOT NULL CHECK (alias_kind IN ('numeric_label', 'canonical_name')),
  PRIMARY KEY (chunk_id, alias)
);

CREATE INDEX idx_chunk_aliases_alias ON chunk_aliases(alias);

CREATE TABLE chunk_references (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
  matched_text TEXT NOT NULL,
  span_start INTEGER NOT NULL,
  span_end INTEGER NOT NULL,
  ref_kind TEXT NOT NULL CHECK (ref_kind IN ('numeric', 'name_phrase')),
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_chunk_refs_source ON chunk_references(source_chunk_id);
CREATE INDEX idx_chunk_refs_matched ON chunk_references(matched_text);
