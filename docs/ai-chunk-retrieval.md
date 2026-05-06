# AI Chunk Retrieval

Gloss should let the AI work with very large PDFs without ever trying to load an entire document into the model context window. The core idea is simple: search locally over chunked document data, pick a very small set of relevant chunks, and only then hand that reduced context to the chat model.

## Problem

When a user says "find where this is defined" or "which chunk proves this?", the naive approach would be to send the whole PDF text to the LLM. That stops scaling quickly:

- Large documents will exceed practical context limits.
- Even if the model can technically accept a long prompt, cost and latency get worse as the prompt grows.
- Big prompts make answers less predictable because relevant material is diluted by irrelevant pages.

So "the AI can search a huge document" should not mean "the model reads the whole document every time". It should mean "the app retrieves a few likely chunks from the local database, and the model answers from those".

## Current Data Model

Gloss already has the right primitive for this: `chunks`.

Each chunk is a small semantic unit such as a definition, theorem, proof, exercise, or example. Relevant retrieval inputs already exist:

- Chunk aliases:
  Exact or near-exact symbolic references such as `1.13` or a canonical name.
- Chunk references:
  Cross-references detected inside chunk body text.
- Chunk body text:
  `formatted_body_md` when available, otherwise OCR/transcribed text.
- Glossary text:
  User-authored notes attached to a chunk.
- Current-document scoping:
  Retrieval should default to the open PDF, not the whole library.

That means v1 retrieval can be local, deterministic, and cheap: search the chunks table plus alias data for the current document.

## V1 Retrieval Flow

The intended v1 flow is:

1. The user asks a natural-language question in chunk chat or page chat.
2. A small retrieval decision step determines whether search is useful for this turn.
3. The app runs a local backend command such as `search_chunks_for_ai(...)` against the current document only.
4. Matching chunks are ranked.
5. Only the top few hits are formatted as short summaries/snippets.
6. Those hits are injected into the streamed AI prompt as supplemental context.
7. The model answers using the active chunk/page context plus the retrieved chunk set.

Important points:

- The search input is a natural-language query.
- The search output is a small ranked list of chunk summaries/snippets.
- The model never sees the whole document unless a future feature explicitly asks for more pages.

## Why This Scales

This scales well because the expensive part is bounded:

- Search happens over chunks, not over full-page images.
- Retrieval is local and cheap compared with LLM inference.
- Prompt size stays bounded because only a few results are injected.
- Latency is predictable because retrieval happens before streaming, not after the model has already produced a large answer.

In practice, the app can search hundreds of chunks locally and still only show the model four or five of them.

## V1 Ranking

The ranking should stay simple and interpretable in v1:

1. Exact alias matches:
   For symbolic lookups like `1.13`.
2. Exact title or subject matches:
   For direct references to theorem names or proof subjects.
3. Alias token overlap:
   Useful when the query partially names a known chunk label.
4. Title/subject token overlap:
   Useful for natural-language queries like "linear independence criterion".
5. Body/glossary text overlap:
   Fallback when the key terms appear in chunk text or attached notes.

The important thing is not perfect ranking. It is that the ranking is explainable, fast, and easy to improve later.

## Limitations

Lexical retrieval has real limits:

- It is sensitive to wording.
- It misses notation changes and synonyms more easily than semantic retrieval.
- It works best when chunks have good titles and aliases.
- It is current-document-first, so it does not solve cross-library retrieval by itself.
- It does not understand deeper mathematical equivalence on its own.

That is acceptable for v1 because the goal is to make chunk-aware AI useful now, not to solve full semantic recall on day one.

## Future Path A: Semantic Tags Alongside Aliases

Aliases and semantic tags solve different problems and should coexist.

Aliases are for symbolic or explicit references:

- `1.13`
- `Theorem 2.4`
- `Linear dependence lemma`

Semantic tags are for concept-level retrieval:

- `linear independence criterion`
- `span minimality`
- `kernel/image relationship`

They help when:

- The user uses different wording from the source text.
- The book uses one notation and the user uses another.
- Several chunks discuss the same concept from different angles.

Tags can be curated, AI-generated, or both. The important design point is that tags should be treated as another retrieval layer, not a replacement for aliases.

## Future Path B: Embeddings

Embeddings become worthwhile when lexical retrieval starts missing too many conceptually relevant chunks.

The typical shape would be:

- Generate an embedding for each chunk body, title, and maybe glossary text.
- Store those vectors locally.
- Embed the user query at request time.
- Run nearest-neighbour search to get semantically similar chunks.
- Combine vector hits with lexical hits and rerank.

Embeddings are especially useful for:

- Synonyms
- Rephrasings
- Notation variation
- Cross-source concept recall

They do introduce more moving parts:

- Background generation jobs
- Storage for vectors
- Versioning when chunk text changes
- Provider/model choices

So embeddings should be added when the lexical system is clearly useful but clearly not enough.

## Recommended Long-Term Shape

The best long-term design is hybrid retrieval:

- `Aliases`:
  Exact or near-exact symbolic references like `1.13`.
- `Semantic tags`:
  Curated or generated concept labels like `linear independence criterion`.
- `Embeddings`:
  Dense vector similarity for broader conceptual recall.

These layers complement each other:

- Aliases give precision.
- Tags give structured concept recall.
- Embeddings give fuzzy semantic recall.

The likely steady-state architecture is:

1. Run cheap lexical retrieval first.
2. Blend in tag matches.
3. Add embedding hits where they improve recall.
4. Rerank and pass only a tiny top slice into the LLM prompt.

That preserves the product goal: the AI feels like it can search a huge document, but the system remains fast, bounded, and chunk-aware.
