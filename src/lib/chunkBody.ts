import katex from "katex";

type ChunkBodyBlock =
  | { kind: "paragraph"; lines: string[] }
  | { kind: "display-math"; tex: string }
  | { kind: "heading"; level: number; text: string }
  | { kind: "code-fence"; code: string; language: string | null }
  | { kind: "ordered-list"; items: string[]; start: number }
  | { kind: "unordered-list"; items: string[] };

interface DisplayMathMatch {
  nextIndex: number;
  tex: string;
}

export interface RenderChunkBodyOptions {
  copyCodeBlocks?: boolean;
  allowHeadings?: boolean;
  allowStrong?: boolean;
  suppressLeadingText?: string[];
}

const DISPLAY_ENVIRONMENTS = new Set([
  "equation",
  "equation*",
  "align",
  "align*",
  "alignat",
  "alignat*",
  "gather",
  "gather*",
  "CD",
]);

const orderedListPattern = /^\s*(\d+)\.\s+(.*)$/;
const unorderedListPattern = /^\s*[-*]\s+(.*)$/;
const displayEnvironmentPattern = /^\\begin\{([a-zA-Z*]+)\}/;
const headingPattern = /^\s{0,3}(#{1,6})\s+(.+?)\s*#*\s*$/;
const codeFencePattern = /^\s*```([A-Za-z0-9_-]+)?\s*$/;
const SAFE_LINK_PATTERN = /^(https?:\/\/|mailto:|gloss-chunk:\d+)/i;
const GLOSS_CHUNK_HREF_PATTERN = /^gloss-chunk:(\d+)$/i;
const WORD_CHAR_PATTERN = /[A-Za-z0-9]/;
const MARKDOWN_ESCAPABLE_CHARS = new Set(["\\", "`", "*", "[", "]", "(", ")", "~"]);
const OCR_BBOX_METADATA_LINE_PATTERN =
  /^\s*(?:!\[[^\]]*]\(\s*)?page\s*=\s*\d+\s*,\s*bbox\s*=\s*\[[^\]]+]\s*\)?\s*$/i;

export interface ResolvedReference {
  matched_text: string;
  span_start: number;
  span_end: number;
  target_id: number;
}

export function renderChunkBodyHtml(
  source: string,
  references?: ResolvedReference[],
  options: RenderChunkBodyOptions = {},
): string {
  const prepared = injectReferenceLinks(source, references);
  const normalized = prepared.replace(/\r\n?/g, "\n").trim();
  if (!normalized) return "";
  const blocks = parseBlocks(normalized, options);
  const visibleBlocks = suppressLeadingDuplicateBlocks(blocks, options);
  return visibleBlocks.map((block) => renderBlock(block, options)).join("");
}

// Splice markdown links into the source at each resolved reference span. Byte
// offsets come from the Rust extractor and refer to the original `formatted_body_md`
// string; the frontend treats them as UTF-16 code-unit offsets, which agree for
// ASCII content. Done descending so earlier offsets stay valid after each splice.
function injectReferenceLinks(
  source: string,
  references?: ResolvedReference[],
): string {
  if (!references || references.length === 0) return source;
  const encoder = typeof TextEncoder !== "undefined" ? new TextEncoder() : null;
  const sorted = [...references]
    .filter(
      (r) =>
        Number.isInteger(r.target_id)
        && r.span_end > r.span_start
        && r.span_start >= 0,
    )
    .sort((a, b) => b.span_start - a.span_start);
  if (sorted.length === 0) return source;

  // The Rust extractor reports byte offsets into the UTF-8 source. Build a
  // byte-offset → UTF-16-code-unit map once, then translate each reference.
  let byteIndex = new Uint32Array(0);
  if (encoder) {
    const bytes = encoder.encode(source);
    byteIndex = new Uint32Array(bytes.length + 1);
    let byteCursor = 0;
    for (let i = 0; i < source.length; ) {
      const codePoint = source.codePointAt(i) ?? 0;
      const utf16Len = codePoint > 0xffff ? 2 : 1;
      const utf8Len =
        codePoint < 0x80
          ? 1
          : codePoint < 0x800
            ? 2
            : codePoint < 0x10000
              ? 3
              : 4;
      for (let k = 0; k < utf8Len; k += 1) {
        byteIndex[byteCursor + k] = i;
      }
      byteCursor += utf8Len;
      i += utf16Len;
    }
    byteIndex[bytes.length] = source.length;
  }

  const byteToChar = (offset: number): number => {
    if (!encoder) return offset;
    if (offset < 0) return 0;
    if (offset >= byteIndex.length) return source.length;
    return byteIndex[offset];
  };

  let out = source;
  for (const ref of sorted) {
    const start = byteToChar(ref.span_start);
    const end = byteToChar(ref.span_end);
    if (end <= start) continue;
    const label = out.slice(start, end);
    const escapedLabel = label.replace(/([\\[\]])/g, "\\$1");
    const replacement = `[${escapedLabel}](gloss-chunk:${ref.target_id})`;
    out = out.slice(0, start) + replacement + out.slice(end);
  }
  return out;
}

function parseBlocks(source: string, options: RenderChunkBodyOptions): ChunkBodyBlock[] {
  const blocks: ChunkBodyBlock[] = [];
  const lines = source.split("\n");
  const allowHeadings = options.allowHeadings !== false;
  let paragraphLines: string[] = [];
  let activeOrderedList: { items: string[]; start: number } | null = null;
  let activeUnorderedList: string[] | null = null;
  let activeCodeFence: { language: string | null; lines: string[] } | null = null;

  const flushParagraph = () => {
    if (!paragraphLines.length) return;
    blocks.push({ kind: "paragraph", lines: paragraphLines });
    paragraphLines = [];
  };

  const flushLists = () => {
    if (activeOrderedList) {
      blocks.push({
        kind: "ordered-list",
        items: activeOrderedList.items,
        start: activeOrderedList.start,
      });
      activeOrderedList = null;
    }
    if (activeUnorderedList) {
      blocks.push({ kind: "unordered-list", items: activeUnorderedList });
      activeUnorderedList = null;
    }
  };

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const trimmed = line.trim();

    if (activeCodeFence) {
      const codeFenceEnd = line.match(codeFencePattern);
      if (codeFenceEnd) {
        blocks.push({
          kind: "code-fence",
          code: activeCodeFence.lines.join("\n"),
          language: activeCodeFence.language,
        });
        activeCodeFence = null;
      } else {
        activeCodeFence.lines.push(line);
      }
      continue;
    }

    if (!trimmed) {
      flushParagraph();
      flushLists();
      continue;
    }

    const codeFenceStart = line.match(codeFencePattern);
    if (codeFenceStart) {
      flushParagraph();
      flushLists();
      activeCodeFence = {
        language: codeFenceStart[1] ?? null,
        lines: [],
      };
      continue;
    }

    const displayMath = consumeDisplayMath(lines, index);
    if (displayMath) {
      flushParagraph();
      flushLists();
      blocks.push({ kind: "display-math", tex: displayMath.tex });
      index = displayMath.nextIndex;
      continue;
    }

    const heading = line.match(headingPattern);
    if (heading) {
      flushParagraph();
      flushLists();
      if (allowHeadings) {
        blocks.push({
          kind: "heading",
          level: heading[1].length,
          text: heading[2].trim(),
        });
      } else {
        blocks.push({
          kind: "paragraph",
          lines: [heading[2].trim()],
        });
      }
      continue;
    }

    const orderedItem = line.match(orderedListPattern);
    if (orderedItem) {
      flushParagraph();
      if (activeUnorderedList) {
        blocks.push({ kind: "unordered-list", items: activeUnorderedList });
        activeUnorderedList = null;
      }
      if (!activeOrderedList) {
        activeOrderedList = {
          items: [],
          start: Number(orderedItem[1]),
        };
      }
      activeOrderedList.items.push(orderedItem[2].trim());
      continue;
    }

    const unorderedItem = line.match(unorderedListPattern);
    if (unorderedItem) {
      flushParagraph();
      if (activeOrderedList) {
        blocks.push({
          kind: "ordered-list",
          items: activeOrderedList.items,
          start: activeOrderedList.start,
        });
        activeOrderedList = null;
      }
      if (!activeUnorderedList) {
        activeUnorderedList = [];
      }
      activeUnorderedList.push(unorderedItem[1].trim());
      continue;
    }

    flushLists();
    paragraphLines.push(line);
  }

  flushParagraph();
  flushLists();
  if (activeCodeFence) {
    blocks.push({
      kind: "code-fence",
      code: activeCodeFence.lines.join("\n"),
      language: activeCodeFence.language,
    });
  }
  return blocks;
}

function consumeDisplayMath(lines: string[], startIndex: number): DisplayMathMatch | null {
  const firstLine = lines[startIndex].trim();
  if (!firstLine) return null;

  const delimiterMatch = matchDisplayDelimiter(firstLine, "$$", "$$");
  if (delimiterMatch) return delimiterMatch;

  const bracketMatch = matchDisplayDelimiter(firstLine, "\\[", "\\]");
  if (bracketMatch) return bracketMatch;

  const environmentMatch = firstLine.match(displayEnvironmentPattern);
  if (!environmentMatch) return null;

  const environment = environmentMatch[1];
  if (!DISPLAY_ENVIRONMENTS.has(environment)) return null;

  const endToken = `\\end{${environment}}`;
  if (firstLine.includes(endToken)) {
    return { nextIndex: startIndex, tex: firstLine };
  }

  const collected = [lines[startIndex]];
  for (let index = startIndex + 1; index < lines.length; index += 1) {
    collected.push(lines[index]);
    if (lines[index].includes(endToken)) {
      return {
        nextIndex: index,
        tex: collected.join("\n").trim(),
      };
    }
  }

  return null;

  function matchDisplayDelimiter(
    line: string,
    opener: string,
    closer: string,
  ): DisplayMathMatch | null {
    if (!line.startsWith(opener)) return null;

    const inner = line.slice(opener.length);
    if (inner.endsWith(closer) && inner !== closer) {
      return {
        nextIndex: startIndex,
        tex: inner.slice(0, inner.length - closer.length).trim(),
      };
    }

    const collected: string[] = inner.trim() ? [inner] : [];
    for (let index = startIndex + 1; index < lines.length; index += 1) {
      const current = lines[index];
      const trimmedCurrent = current.trim();
      if (trimmedCurrent === closer) {
        return {
          nextIndex: index,
          tex: collected.join("\n").trim(),
        };
      }
      if (trimmedCurrent.endsWith(closer)) {
        collected.push(current.slice(0, current.lastIndexOf(closer)));
        return {
          nextIndex: index,
          tex: collected.join("\n").trim(),
        };
      }
      collected.push(current);
    }

    return null;
  }
}

function renderBlock(block: ChunkBodyBlock, options: RenderChunkBodyOptions): string {
  switch (block.kind) {
    case "paragraph": {
      const text = block.lines.map((line) => line.trim()).join(" ");
      if (isOcrBboxMetadataLine(text)) return "";
      return `<p>${renderInlineContent(text, options)}</p>`;
    }
    case "display-math":
      return `<div class="chunk-math-display">${renderMath(block.tex, true)}</div>`;
    case "heading":
      if (options.allowHeadings === false) return `<p>${renderInlineContent(block.text, options)}</p>`;
      return `<h${block.level}>${renderInlineContent(block.text, options)}</h${block.level}>`;
    case "code-fence": {
      const languageClass = block.language
        ? ` class="language-${escapeHtmlAttribute(block.language)}"`
        : "";
      const code = `<pre><code${languageClass}>${escapeHtml(block.code)}</code></pre>`;
      if (!options.copyCodeBlocks) return code;

      const languageLabel = block.language
        ? `<span class="chunk-code-language">${escapeHtml(block.language)}</span>`
        : `<span class="chunk-code-language" aria-hidden="true"></span>`;
      return `<div class="chunk-code-block"><div class="chunk-code-toolbar">${languageLabel}<button class="chunk-code-copy" type="button">Copy</button></div>${code}</div>`;
    }
    case "ordered-list":
      return `<ol start="${block.start}">${block.items
        .map((item) => `<li>${renderInlineContent(item, options)}</li>`)
        .join("")}</ol>`;
    case "unordered-list":
      return `<ul>${block.items
        .map((item) => `<li>${renderInlineContent(item, options)}</li>`)
        .join("")}</ul>`;
  }
}

function suppressLeadingDuplicateBlocks(
  blocks: ChunkBodyBlock[],
  options: RenderChunkBodyOptions,
): ChunkBodyBlock[] {
  const candidateSet = new Set(
    (options.suppressLeadingText ?? [])
      .map((value) => normalizeTextForHeadingMatch(value))
      .filter((value) => value.length > 0),
  );
  if (candidateSet.size === 0) return blocks;

  let index = 0;
  let removed = 0;
  while (index < blocks.length && removed < 3) {
    const block = blocks[index];
    if (block.kind !== "heading" && block.kind !== "paragraph") break;
    const blockText = normalizeTextForHeadingMatch(
      block.kind === "heading"
        ? block.text
        : block.lines.map((line) => line.trim()).join(" "),
    );
    if (!isMatchingSuppressedHeading(blockText, candidateSet)) break;
    index += 1;
    removed += 1;
  }

  return index > 0 ? blocks.slice(index) : blocks;
}

function isMatchingSuppressedHeading(
  blockText: string,
  candidates: Set<string>,
): boolean {
  if (!blockText) return false;
  if (candidates.has(blockText)) return true;
  return [...candidates].some((candidate) =>
    blockText === `${candidate}.`
    || blockText === `${candidate}:`
    || blockText === `${candidate};`,
  );
}

function normalizeTextForHeadingMatch(source: string): string {
  let value = source.trim();
  if (!value) return "";
  value = value.replace(/^#{1,6}\s+/u, "");
  value = value.replace(/^(\*\*|__)(.*)\1$/u, "$2");
  value = value.replace(/^([*_])(.+)\1$/u, "$2");
  return value
    .replace(/\s+/gu, " ")
    .trim()
    .toLocaleLowerCase();
}

function isOcrBboxMetadataLine(source: string): boolean {
  return OCR_BBOX_METADATA_LINE_PATTERN.test(source.trim());
}

function renderInlineContent(source: string, options: RenderChunkBodyOptions = {}): string {
  let index = 0;
  let protectedSource = "";
  const placeholders = new Map<string, string>();

  while (index < source.length) {
    if (
      source[index] === "\\"
      && MARKDOWN_ESCAPABLE_CHARS.has(source[index + 1] ?? "")
    ) {
      protectedSource += reservePlaceholder(escapeHtml(source[index + 1]), placeholders);
      index += 2;
      continue;
    }

    const markdownLink = consumeMarkdownLink(source, index);
    if (markdownLink) {
      const safeHref = sanitizeHref(markdownLink.href);
      if (safeHref) {
        const chunkMatch = safeHref.match(GLOSS_CHUNK_HREF_PATTERN);
        const linkHtml = chunkMatch
          ? `<a class="chunk-xref" data-chunk-id="${escapeHtmlAttribute(chunkMatch[1])}" role="button" tabindex="0">${renderInlineContent(markdownLink.label, options)}</a>`
          : `<a href="${escapeHtmlAttribute(safeHref)}" target="_blank" rel="noreferrer noopener">${renderInlineContent(markdownLink.label, options)}</a>`;
        protectedSource += reservePlaceholder(linkHtml, placeholders);
        index = markdownLink.nextIndex;
        continue;
      }
    }

    if (source.startsWith("\\(", index) && !isEscaped(source, index)) {
      const end = source.indexOf("\\)", index + 2);
      if (end !== -1) {
        protectedSource += reservePlaceholder(
          renderMath(source.slice(index + 2, end), false),
          placeholders,
        );
        index = end + 2;
        continue;
      }
    }

    if (source[index] === "$" && !isEscaped(source, index) && source[index + 1] !== "$") {
      const end = findClosingInlineDollar(source, index + 1);
      if (end !== -1) {
        protectedSource += reservePlaceholder(
          renderMath(source.slice(index + 1, end), false),
          placeholders,
        );
        index = end + 1;
        continue;
      }
    }

    if (source[index] === "`") {
      const end = findClosingBacktick(source, index + 1);
      if (end !== -1) {
        protectedSource += reservePlaceholder(
          `<code>${escapeHtml(source.slice(index + 1, end))}</code>`,
          placeholders,
        );
        index = end + 1;
        continue;
      }
    }

    protectedSource += source[index];
    index += 1;
  }

  const formatted = renderMarkdownSpans(escapeHtml(protectedSource), options);
  return restorePlaceholders(formatted, placeholders);
}

function renderMath(source: string, displayMode: boolean): string {
  return katex.renderToString(source.trim(), {
    displayMode,
    output: "htmlAndMathml",
    strict: "ignore",
    throwOnError: false,
    trust: false,
  });
}

function findClosingInlineDollar(source: string, startIndex: number): number {
  for (let index = startIndex; index < source.length; index += 1) {
    if (source[index] !== "$" || isEscaped(source, index) || source[index + 1] === "$") continue;
    return index;
  }
  return -1;
}

function findClosingBacktick(source: string, startIndex: number): number {
  for (let index = startIndex; index < source.length; index += 1) {
    if (source[index] === "`" && !isEscaped(source, index)) return index;
  }
  return -1;
}

function renderMarkdownSpans(source: string, options: RenderChunkBodyOptions = {}): string {
  const markers = [
    { marker: "**", tag: "strong" },
    { marker: "~~", tag: "del" },
    { marker: "*", tag: "em" },
  ] as const;

  let output = "";
  let textStart = 0;
  let index = 0;

  while (index < source.length) {
    const format = markers.find(({ marker }) => source.startsWith(marker, index));
    if (!format || !canOpenMarkdownMarker(source, index, format.marker)) {
      index += 1;
      continue;
    }

    const end = findClosingMarkdownMarker(source, format.marker, index + format.marker.length);
    if (end === -1) {
      index += format.marker.length;
      continue;
    }

    const inner = source.slice(index + format.marker.length, end);
    const renderedInner = renderMarkdownSpans(inner, options);
    const skipTag = format.tag === "strong" && options.allowStrong === false;
    output += source.slice(textStart, index);
    output += skipTag ? renderedInner : `<${format.tag}>${renderedInner}</${format.tag}>`;
    index = end + format.marker.length;
    textStart = index;
  }

  output += source.slice(textStart);
  return output;
}

function findClosingMarkdownMarker(
  source: string,
  marker: string,
  startIndex: number,
): number {
  for (let index = startIndex; index <= source.length - marker.length; index += 1) {
    if (!source.startsWith(marker, index) || isEscaped(source, index)) continue;
    if (!canCloseMarkdownMarker(source, index, marker)) continue;
    const inner = source.slice(startIndex, index);
    if (!inner || /^\s|\s$/u.test(inner)) continue;
    return index;
  }
  return -1;
}

function canOpenMarkdownMarker(source: string, index: number, marker: string): boolean {
  const next = source[index + marker.length] ?? "";
  if (!next || /\s/u.test(next)) return false;
  const previous = source[index - 1] ?? "";
  return !WORD_CHAR_PATTERN.test(previous);
}

function canCloseMarkdownMarker(source: string, index: number, marker: string): boolean {
  const previous = source[index - 1] ?? "";
  if (!previous || /\s/u.test(previous)) return false;
  const next = source[index + marker.length] ?? "";
  return !WORD_CHAR_PATTERN.test(next);
}

interface MarkdownLinkMatch {
  label: string;
  href: string;
  nextIndex: number;
}

function consumeMarkdownLink(source: string, startIndex: number): MarkdownLinkMatch | null {
  if (source[startIndex] !== "[") return null;

  let labelEnd = -1;
  let labelDepth = 0;
  for (let index = startIndex + 1; index < source.length; index += 1) {
    if (source[index] === "\\") {
      index += 1;
      continue;
    }
    if (source[index] === "[") {
      labelDepth += 1;
      continue;
    }
    if (source[index] !== "]") continue;
    if (labelDepth === 0) {
      labelEnd = index;
      break;
    }
    labelDepth -= 1;
  }

  if (labelEnd === -1 || source[labelEnd + 1] !== "(") return null;

  let hrefEnd = -1;
  let hrefDepth = 0;
  for (let index = labelEnd + 2; index < source.length; index += 1) {
    if (source[index] === "\\") {
      index += 1;
      continue;
    }
    if (source[index] === "(") {
      hrefDepth += 1;
      continue;
    }
    if (source[index] !== ")") continue;
    if (hrefDepth === 0) {
      hrefEnd = index;
      break;
    }
    hrefDepth -= 1;
  }

  if (hrefEnd === -1) return null;

  return {
    label: source.slice(startIndex + 1, labelEnd),
    href: source.slice(labelEnd + 2, hrefEnd),
    nextIndex: hrefEnd + 1,
  };
}

function sanitizeHref(source: string): string | null {
  const trimmed = source.trim();
  if (!trimmed || !SAFE_LINK_PATTERN.test(trimmed)) return null;
  return trimmed;
}

function reservePlaceholder(html: string, placeholders: Map<string, string>): string {
  const token = `\u0000${placeholders.size}\u0000`;
  placeholders.set(token, html);
  return token;
}

function restorePlaceholders(source: string, placeholders: Map<string, string>): string {
  let restored = source;
  for (const [token, html] of placeholders) {
    restored = restored.replaceAll(token, html);
  }
  return restored;
}

function isEscaped(source: string, index: number): boolean {
  let backslashCount = 0;
  for (let cursor = index - 1; cursor >= 0 && source[cursor] === "\\"; cursor -= 1) {
    backslashCount += 1;
  }
  return backslashCount % 2 === 1;
}

function escapeHtml(source: string): string {
  return source
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function escapeHtmlAttribute(source: string): string {
  return escapeHtml(source);
}
