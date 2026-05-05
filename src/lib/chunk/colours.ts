export interface ChunkColour {
  accent: string;
  tint: string;
  label: string;
  short: string;
}

export const FALLBACK_CHUNK_COLOUR: ChunkColour = {
  accent: "oklch(0.52 0.03 240)",
  tint: "oklch(0.975 0.008 240)",
  label: "Chunk",
  short: "?",
};

export const CHUNK_COLOURS: Record<string, ChunkColour> = {
  definition: { accent: "oklch(0.50 0.17 233)", tint: "oklch(0.965 0.032 233)", label: "Definition", short: "Def" },
  theorem: { accent: "oklch(0.47 0.17 290)", tint: "oklch(0.965 0.032 290)", label: "Theorem", short: "Thm" },
  proof: { accent: "oklch(0.50 0.10 180)", tint: "oklch(0.975 0.020 180)", label: "Proof", short: "Prf" },
  exercise: { accent: "oklch(0.58 0.16 50)", tint: "oklch(0.970 0.032 50)", label: "Exercise", short: "Ex" },
  example: { accent: "oklch(0.58 0.16 50)", tint: "oklch(0.970 0.032 50)", label: "Example", short: "Eg" },
  explanation: { accent: "oklch(0.52 0.03 240)", tint: "oklch(0.975 0.008 240)", label: "Explanation", short: "Exp" },
  question: { accent: "oklch(0.56 0.16 22)", tint: "oklch(0.972 0.022 22)", label: "Question", short: "Q" },
};

export function getChunkColour(chunkType: string): ChunkColour {
  return CHUNK_COLOURS[chunkType] ?? FALLBACK_CHUNK_COLOUR;
}
