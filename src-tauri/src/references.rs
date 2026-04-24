// Cross-reference extractor.
//
// For a given chunk body (markdown with inline/display math), find all spans
// that match a known alias of another chunk in the same document. Numeric
// labels ("2.19") match the alias exactly; canonical names ("Linear dependence
// lemma") match case-insensitively. Matches inside math / code fences are
// skipped so "$x_{2.19}$" is not linkified.
//
// Resolution to a target chunk id happens at read time via a JOIN through
// chunk_aliases, so this module only needs to know which strings to look for.

use log::{debug, info};
use sqlx::{Row, SqlitePool};
use std::collections::HashSet;

const LOG_TARGET: &str = "gloss_lib::references";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    Numeric,
    NamePhrase,
}

impl RefKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Numeric => "numeric",
            Self::NamePhrase => "name_phrase",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtractedRef {
    pub matched_text: String,
    pub span_start: usize,
    pub span_end: usize,
    pub ref_kind: RefKind,
}

/// Document-wide alias index built from `chunk_aliases`. Cheap to construct;
/// regenerated on every reindex pass.
pub struct AliasIndex {
    numeric_aliases: HashSet<String>,
    // canonical_name aliases, ASCII-lowercased, sorted longest-first so the
    // name-phrase pass prefers longer matches ("Linear dependence lemma"
    // before "dependence").
    name_aliases_lower: Vec<String>,
}

impl AliasIndex {
    pub fn from_aliases(aliases: &[(String, String)]) -> Self {
        let mut numeric: HashSet<String> = HashSet::new();
        let mut names: Vec<String> = Vec::new();
        for (text, kind) in aliases {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            match kind.as_str() {
                "numeric_label" => {
                    numeric.insert(trimmed.to_string());
                }
                "canonical_name" => {
                    names.push(trimmed.to_ascii_lowercase());
                }
                _ => {}
            }
        }
        names.sort();
        names.dedup();
        names.sort_by(|a, b| b.len().cmp(&a.len()));
        Self {
            numeric_aliases: numeric,
            name_aliases_lower: names,
        }
    }

    fn is_empty(&self) -> bool {
        self.numeric_aliases.is_empty() && self.name_aliases_lower.is_empty()
    }
}

/// Scan `body` for references. Returns byte offsets into `body`, sorted by
/// `span_start`. Does not resolve to target chunk ids — that happens at
/// read time via the aliases table.
pub fn extract_references(body: &str, index: &AliasIndex) -> Vec<ExtractedRef> {
    if body.is_empty() || index.is_empty() {
        return Vec::new();
    }
    let unsafe_ranges = compute_unsafe_ranges(body);
    let bytes = body.as_bytes();
    let mut out: Vec<ExtractedRef> = Vec::new();

    // ── Numeric pass ────────────────────────────────────────────────────
    // Match \b\d+(\.\d+)+\b outside unsafe ranges; also handle an optional
    // keyword prefix (Lemma 2.19 etc.) by letting the number match on its
    // own. We just record the number span; keyword prefix styling lives in
    // the frontend.
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_digit() && !is_word_char_byte(prev_byte(bytes, i)) {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let mut has_dot = false;
            while i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                i += 1; // consume .
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                has_dot = true;
            }
            let end = i;
            let followed_by_word = end < bytes.len() && is_word_char_byte(Some(bytes[end]));
            if has_dot && !followed_by_word && !overlaps_any(start, end, &unsafe_ranges) {
                let matched = &body[start..end];
                if index.numeric_aliases.contains(matched) {
                    out.push(ExtractedRef {
                        matched_text: matched.to_string(),
                        span_start: start,
                        span_end: end,
                        ref_kind: RefKind::Numeric,
                    });
                }
            }
            continue;
        }
        i += 1;
    }

    // ── Name-phrase pass ────────────────────────────────────────────────
    // Lowercase the whole body once for case-insensitive matching.
    // `to_ascii_lowercase` touches only ASCII A-Z bytes and preserves every
    // other byte verbatim, so byte offsets in `body_lower` match `body` 1:1
    // even when the body contains multi-byte UTF-8 (math symbols, etc.).
    if !index.name_aliases_lower.is_empty() {
        let body_lower = body.to_ascii_lowercase();
        let lower_bytes = body_lower.as_bytes();

        let mut i = 0;
        while i < lower_bytes.len() {
            // Only start matches at a word boundary.
            if is_word_char_byte(prev_byte(lower_bytes, i)) {
                i += 1;
                continue;
            }
            let mut matched = false;
            for alias in &index.name_aliases_lower {
                let alias_bytes = alias.as_bytes();
                if lower_bytes.len() - i < alias_bytes.len() {
                    continue;
                }
                if &lower_bytes[i..i + alias_bytes.len()] != alias_bytes {
                    continue;
                }
                let end = i + alias_bytes.len();
                // Must end at a word boundary.
                if is_word_char_byte(lower_bytes.get(end).copied()) {
                    continue;
                }
                if overlaps_any(i, end, &unsafe_ranges) {
                    continue;
                }
                // Numeric refs already recorded take priority — skip overlaps.
                if out.iter().any(|r| r.span_start < end && r.span_end > i) {
                    continue;
                }
                // Preserve original casing from body.
                let matched_text = body[i..end].to_string();
                out.push(ExtractedRef {
                    matched_text,
                    span_start: i,
                    span_end: end,
                    ref_kind: RefKind::NamePhrase,
                });
                i = end;
                matched = true;
                break;
            }
            if !matched {
                i += 1;
            }
        }
    }

    out.sort_by_key(|r| r.span_start);
    out
}

fn prev_byte(bytes: &[u8], i: usize) -> Option<u8> {
    if i == 0 {
        None
    } else {
        Some(bytes[i - 1])
    }
}

fn is_word_char_byte(b: Option<u8>) -> bool {
    matches!(b, Some(c) if c.is_ascii_alphanumeric() || c == b'_')
}

fn overlaps_any(start: usize, end: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|(s, e)| start < *e && *s < end)
}

/// Byte ranges of the body where linkification must be skipped. Covers
/// inline / display math and code blocks, mirroring the protection done by
/// the frontend renderer in src/lib/chunkBody.ts.
fn compute_unsafe_ranges(body: &str) -> Vec<(usize, usize)> {
    let bytes = body.as_bytes();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Triple-backtick code fence.
        if bytes[i..].starts_with(b"```") {
            let start = i;
            i += 3;
            while i + 2 < bytes.len() && !bytes[i..].starts_with(b"```") {
                i += 1;
            }
            if bytes[i..].starts_with(b"```") {
                i += 3;
            } else {
                i = bytes.len();
            }
            ranges.push((start, i));
            continue;
        }
        // Inline code.
        if bytes[i] == b'`' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'`' && bytes[i] != b'\n' {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            ranges.push((start, i));
            continue;
        }
        // Display math $$...$$.
        if bytes[i..].starts_with(b"$$") {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !bytes[i..].starts_with(b"$$") {
                i += 1;
            }
            if bytes[i..].starts_with(b"$$") {
                i += 2;
            } else {
                i = bytes.len();
            }
            ranges.push((start, i));
            continue;
        }
        // Inline math $...$ (single $ not preceded by backslash).
        if bytes[i] == b'$' && prev_byte(bytes, i) != Some(b'\\') {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\n' {
                    break;
                }
                if bytes[i] == b'$' && bytes[i - 1] != b'\\' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            ranges.push((start, i));
            continue;
        }
        // \(...\)
        if bytes[i..].starts_with(b"\\(") {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'\\' && bytes[i + 1] == b')') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            }
            ranges.push((start, i));
            continue;
        }
        // \[...\]
        if bytes[i..].starts_with(b"\\[") {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'\\' && bytes[i + 1] == b']') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            }
            ranges.push((start, i));
            continue;
        }
        // \begin{env}...\end{env}
        if bytes[i..].starts_with(b"\\begin{") {
            let start = i;
            let name_start = i + 7;
            let mut j = name_start;
            while j < bytes.len() && bytes[j] != b'}' && bytes[j] != b'\n' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'}' {
                let env_name = &body[name_start..j];
                let end_token = format!("\\end{{{}}}", env_name);
                i = j + 1;
                while i < bytes.len() {
                    if body[i..].starts_with(&end_token) {
                        i += end_token.len();
                        break;
                    }
                    i += 1;
                }
                ranges.push((start, i));
                continue;
            }
        }
        i += 1;
    }
    ranges
}

/// Extract a numeric label from the leading part of a chunk title.
/// e.g. "2.19 Linear Dependence Lemma" -> Some("2.19")
/// e.g. "Definition 1.4: Vector Space" -> Some("1.4")
/// e.g. "Exercise 11" -> None (bare integer — too noisy for numeric matching)
/// Only dotted forms are returned; bare integers would cause false positives
/// in body scanning (e.g. "3 cases").
pub fn extract_numeric_label_from_title(title: &str) -> Option<String> {
    let bytes = title.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && !is_word_char_byte(prev_byte(bytes, i)) {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let mut has_dot = false;
            while i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                has_dot = true;
            }
            let end = i;
            let followed_by_word = end < bytes.len() && is_word_char_byte(Some(bytes[end]));
            if has_dot && !followed_by_word {
                return Some(title[start..end].to_string());
            }
            // Skip non-dotted integer, keep scanning.
            continue;
        }
        i += 1;
    }
    None
}

/// Backfill `chunk_aliases` rows for chunks that have a title but no aliases
/// (typical for chunks created before the alias feature shipped). Only inserts
/// a deterministic numeric_label derived from the title — the LLM is not
/// called. Idempotent: INSERT OR IGNORE on (chunk_id, alias).
async fn backfill_aliases_from_titles(
    pool: &SqlitePool,
    document_id: i64,
) -> Result<usize, String> {
    let rows = sqlx::query(
        "SELECT c.id, c.title FROM chunks c \
         WHERE c.source_document_id = ? AND c.title IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM chunk_aliases a WHERE a.chunk_id = c.id)",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let mut inserted = 0usize;
    for row in rows {
        let chunk_id: i64 = row.get("id");
        let title: String = row.get("title");
        if let Some(label) = extract_numeric_label_from_title(&title) {
            sqlx::query(
                "INSERT OR IGNORE INTO chunk_aliases (chunk_id, alias, alias_kind) \
                 VALUES (?, ?, 'numeric_label')",
            )
            .bind(chunk_id)
            .bind(&label)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
            inserted += 1;
            debug!(
                target: LOG_TARGET,
                "backfilled numeric_label={:?} for chunk_id={} from title={:?}",
                label, chunk_id, title
            );
        }
    }
    if inserted > 0 {
        info!(
            target: LOG_TARGET,
            "backfilled {} numeric labels for document_id={}",
            inserted, document_id
        );
    }
    Ok(inserted)
}

/// Rebuild `chunk_references` for every chunk in a document. One transaction.
/// Call after chunking completes or whenever aliases change.
pub async fn reindex_document_references(
    pool: &SqlitePool,
    document_id: i64,
) -> Result<(), String> {
    // Ensure existing chunks (created before the alias feature) pick up a
    // deterministic numeric label before we scan bodies.
    backfill_aliases_from_titles(pool, document_id).await?;

    let chunks = sqlx::query(
        "SELECT id, COALESCE(formatted_body_md, ocr_text) AS body \
         FROM chunks WHERE source_document_id = ?",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    if chunks.is_empty() {
        return Ok(());
    }

    let alias_rows = sqlx::query(
        "SELECT a.alias, a.alias_kind \
         FROM chunk_aliases a JOIN chunks c ON c.id = a.chunk_id \
         WHERE c.source_document_id = ?",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let aliases: Vec<(String, String)> = alias_rows
        .into_iter()
        .map(|r| {
            (
                r.get::<String, _>("alias"),
                r.get::<String, _>("alias_kind"),
            )
        })
        .collect();
    info!(
        target: LOG_TARGET,
        "reindex document_id={} chunks={} aliases_in_db={}",
        document_id,
        chunks.len(),
        aliases.len()
    );
    let index = AliasIndex::from_aliases(&aliases);

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query(
        "DELETE FROM chunk_references WHERE source_chunk_id IN \
         (SELECT id FROM chunks WHERE source_document_id = ?)",
    )
    .bind(document_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    let mut total = 0usize;
    for chunk_row in &chunks {
        let chunk_id: i64 = chunk_row.get("id");
        let body: Option<String> = chunk_row.get("body");
        let Some(body) = body.as_deref() else {
            continue;
        };
        let refs = extract_references(body, &index);
        if !refs.is_empty() {
            info!(
                target: LOG_TARGET,
                "chunk_id={} found {} references: {:?}",
                chunk_id,
                refs.len(),
                refs.iter().map(|r| &r.matched_text).collect::<Vec<_>>()
            );
        }
        for r in refs {
            sqlx::query(
                "INSERT INTO chunk_references \
                 (source_chunk_id, matched_text, span_start, span_end, ref_kind) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(chunk_id)
            .bind(&r.matched_text)
            .bind(r.span_start as i64)
            .bind(r.span_end as i64)
            .bind(r.ref_kind.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            total += 1;
        }
    }

    tx.commit().await.map_err(|e| e.to_string())?;
    debug!(
        target: LOG_TARGET,
        "reindexed document_id={} wrote {} references across {} chunks",
        document_id,
        total,
        chunks.len()
    );
    Ok(())
}

/// Rebuild references for a single chunk. Cheaper than a full document pass;
/// used after `generate_chunk_formatted_body` finishes — the chunk's own body
/// changed, but the document's alias set did not.
pub async fn reindex_chunk_references(pool: &SqlitePool, chunk_id: i64) -> Result<(), String> {
    let row = sqlx::query(
        "SELECT source_document_id, COALESCE(formatted_body_md, ocr_text) AS body \
         FROM chunks WHERE id = ?",
    )
    .bind(chunk_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;

    let Some(row) = row else {
        return Ok(());
    };
    let document_id: i64 = row.get("source_document_id");
    let body: Option<String> = row.get("body");

    backfill_aliases_from_titles(pool, document_id).await?;

    let alias_rows = sqlx::query(
        "SELECT a.alias, a.alias_kind \
         FROM chunk_aliases a JOIN chunks c ON c.id = a.chunk_id \
         WHERE c.source_document_id = ?",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let aliases: Vec<(String, String)> = alias_rows
        .into_iter()
        .map(|r| {
            (
                r.get::<String, _>("alias"),
                r.get::<String, _>("alias_kind"),
            )
        })
        .collect();
    let index = AliasIndex::from_aliases(&aliases);

    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM chunk_references WHERE source_chunk_id = ?")
        .bind(chunk_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(body) = body.as_deref() {
        let refs = extract_references(body, &index);
        for r in refs {
            sqlx::query(
                "INSERT INTO chunk_references \
                 (source_chunk_id, matched_text, span_start, span_end, ref_kind) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(chunk_id)
            .bind(&r.matched_text)
            .bind(r.span_start as i64)
            .bind(r.span_end as i64)
            .bind(r.ref_kind.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idx(numeric: &[&str], names: &[&str]) -> AliasIndex {
        let mut aliases: Vec<(String, String)> = Vec::new();
        for n in numeric {
            aliases.push((n.to_string(), "numeric_label".to_string()));
        }
        for n in names {
            aliases.push((n.to_string(), "canonical_name".to_string()));
        }
        AliasIndex::from_aliases(&aliases)
    }

    #[test]
    fn matches_numeric_label() {
        let refs = extract_references("we apply 2.19 here", &idx(&["2.19"], &[]));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].matched_text, "2.19");
        assert_eq!(refs[0].ref_kind, RefKind::Numeric);
    }

    #[test]
    fn ignores_unknown_numeric() {
        let refs = extract_references("weighs 2.5 kg", &idx(&["2.19"], &[]));
        assert!(refs.is_empty());
    }

    #[test]
    fn skips_numeric_inside_inline_math() {
        let refs = extract_references("see $x_{2.19}$ above", &idx(&["2.19"], &[]));
        assert!(refs.is_empty());
    }

    #[test]
    fn skips_numeric_inside_display_math() {
        let refs = extract_references("$$a_{2.19} = b$$", &idx(&["2.19"], &[]));
        assert!(refs.is_empty());
    }

    #[test]
    fn matches_two_numerics_in_one_body() {
        let refs = extract_references("by 2.19 and 2.22", &idx(&["2.19", "2.22"], &[]));
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].matched_text, "2.19");
        assert_eq!(refs[1].matched_text, "2.22");
    }

    #[test]
    fn matches_canonical_name_case_insensitive() {
        let refs = extract_references(
            "by the Linear dependence lemma it follows",
            &idx(&[], &["linear dependence lemma"]),
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].matched_text, "Linear dependence lemma");
        assert_eq!(refs[0].ref_kind, RefKind::NamePhrase);
    }

    #[test]
    fn prefers_longest_name_match() {
        let refs = extract_references(
            "the fundamental theorem of calculus says",
            &idx(
                &[],
                &["fundamental theorem", "fundamental theorem of calculus"],
            ),
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].matched_text, "fundamental theorem of calculus");
    }

    #[test]
    fn skips_name_inside_code_fence() {
        let body = "before\n```\nLinear dependence lemma\n```\nafter";
        let refs = extract_references(body, &idx(&[], &["linear dependence lemma"]));
        assert!(refs.is_empty());
    }

    #[test]
    fn numeric_and_name_in_same_body() {
        let refs = extract_references(
            "by 2.19 (linear dependence lemma) we deduce",
            &idx(&["2.19"], &["linear dependence lemma"]),
        );
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].ref_kind, RefKind::Numeric);
        assert_eq!(refs[1].ref_kind, RefKind::NamePhrase);
    }

    #[test]
    fn requires_word_boundaries_for_name() {
        // "sublemma" should not match "lemma".
        let refs = extract_references("consider sublemmas here", &idx(&[], &["lemma"]));
        assert!(refs.is_empty());
    }

    #[test]
    fn name_match_after_multibyte_char_keeps_correct_offsets() {
        // Regression: previously `body_lower` was built by casting each body
        // byte to a char, which re-encoded any non-ASCII byte as 2 UTF-8 bytes
        // and shifted all downstream offsets. Using `to_ascii_lowercase`
        // preserves byte positions exactly.
        let body = "Let 𝐅 be a field. The finite-dimensional vector space is useful.";
        let refs = extract_references(body, &idx(&[], &["finite-dimensional vector space"]));
        assert_eq!(refs.len(), 1);
        let r = &refs[0];
        assert_eq!(
            &body[r.span_start..r.span_end],
            "finite-dimensional vector space"
        );
    }
}
