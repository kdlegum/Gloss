import { Parser } from "expr-eval";

export type GraphMode = "equation" | "points";

export interface GraphPoint {
  x: number;
  y: number;
}

export interface GraphSpec {
  mode: GraphMode;
  equation: string | null;
  points: GraphPoint[] | null;
  xMin: number;
  xMax: number;
  yMin: number;
  yMax: number;
  lineColour: string;
  lineWidth: number;
}

export interface PlotRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface GraphGeometry {
  plot: PlotRect;
  xAxisY: number | null;
  yAxisX: number | null;
  segments: Array<Array<{ x: number; y: number }>>;
  marks: Array<{ x: number; y: number }>;
  error: string | null;
}

export interface GraphBuildOptions {
  width: number;
  height: number;
  padding?: number;
  samples?: number;
}

const EQUATION_PARSER = new Parser({
  operators: {
    assignment: false,
    logical: false,
    comparison: false,
    in: false,
  },
});

const MIN_GRAPH_LINE_WIDTH = 0.5;
const MAX_GRAPH_LINE_WIDTH = 12;
const DEFAULT_GRAPH_COLOUR = "#2351d1";
const DEFAULT_RANGE = 10;
const DEFAULT_SAMPLES = 240;

export const DEFAULT_GRAPH_SPEC: GraphSpec = {
  mode: "equation",
  equation: "x",
  points: null,
  xMin: -DEFAULT_RANGE,
  xMax: DEFAULT_RANGE,
  yMin: -DEFAULT_RANGE,
  yMax: DEFAULT_RANGE,
  lineColour: DEFAULT_GRAPH_COLOUR,
  lineWidth: 2,
};

export function cloneGraphSpec(spec: GraphSpec): GraphSpec {
  return {
    mode: spec.mode,
    equation: spec.equation,
    points: spec.points?.map((point) => ({ x: point.x, y: point.y })) ?? null,
    xMin: spec.xMin,
    xMax: spec.xMax,
    yMin: spec.yMin,
    yMax: spec.yMax,
    lineColour: spec.lineColour,
    lineWidth: spec.lineWidth,
  };
}

export function normaliseGraphSpec(input: Partial<GraphSpec>): GraphSpec {
  const mode: GraphMode = input.mode === "points" ? "points" : "equation";
  const xMin = finiteOrFallback(input.xMin, DEFAULT_GRAPH_SPEC.xMin);
  const xMax = finiteOrFallback(input.xMax, DEFAULT_GRAPH_SPEC.xMax);
  const yMin = finiteOrFallback(input.yMin, DEFAULT_GRAPH_SPEC.yMin);
  const yMax = finiteOrFallback(input.yMax, DEFAULT_GRAPH_SPEC.yMax);

  const rangeX = xMax > xMin ? [xMin, xMax] : [DEFAULT_GRAPH_SPEC.xMin, DEFAULT_GRAPH_SPEC.xMax];
  const rangeY = yMax > yMin ? [yMin, yMax] : [DEFAULT_GRAPH_SPEC.yMin, DEFAULT_GRAPH_SPEC.yMax];

  const lineWidth = clamp(
    finiteOrFallback(input.lineWidth, DEFAULT_GRAPH_SPEC.lineWidth),
    MIN_GRAPH_LINE_WIDTH,
    MAX_GRAPH_LINE_WIDTH,
  );

  const points = input.points?.filter((point) => Number.isFinite(point.x) && Number.isFinite(point.y)) ?? null;

  return {
    mode,
    equation: sanitizeEquation(input.equation ?? DEFAULT_GRAPH_SPEC.equation ?? ""),
    points: points && points.length > 0 ? points : null,
    xMin: rangeX[0],
    xMax: rangeX[1],
    yMin: rangeY[0],
    yMax: rangeY[1],
    lineColour: sanitizeColour(input.lineColour),
    lineWidth,
  };
}

export function validateGraphSpec(spec: GraphSpec): string | null {
  if (!(spec.xMax > spec.xMin)) return "x range must have xMax > xMin.";
  if (!(spec.yMax > spec.yMin)) return "y range must have yMax > yMin.";
  if (!(spec.lineWidth > 0)) return "Line width must be positive.";

  if (spec.mode === "equation") {
    const equation = sanitizeEquation(spec.equation ?? "");
    if (!equation) return "Equation is required.";
    try {
      EQUATION_PARSER.parse(equation);
    } catch (err) {
      return `Equation error: ${toErrorMessage(err)}`;
    }
    return null;
  }

  if (!spec.points || spec.points.length === 0) {
    return "At least one point is required.";
  }
  return null;
}

export function pointsToJson(points: GraphPoint[] | null): string | null {
  if (!points || points.length === 0) return null;
  return JSON.stringify(points);
}

export function pointsFromJson(value: string | null | undefined): GraphPoint[] | null {
  if (!value?.trim()) return null;
  try {
    const parsed = JSON.parse(value) as unknown;
    if (!Array.isArray(parsed)) return null;
    const points: GraphPoint[] = [];
    for (const entry of parsed) {
      if (!entry || typeof entry !== "object") continue;
      const x = finiteOrNull((entry as { x?: unknown }).x);
      const y = finiteOrNull((entry as { y?: unknown }).y);
      if (x == null || y == null) continue;
      points.push({ x, y });
    }
    return points.length > 0 ? points : null;
  } catch {
    return null;
  }
}

export function pointsToText(points: GraphPoint[] | null): string {
  if (!points || points.length === 0) return "";
  return points
    .map((point) => `${trimNumber(point.x)}, ${trimNumber(point.y)}`)
    .join("\n");
}

export function parsePointsText(source: string): { points: GraphPoint[] | null; error: string | null } {
  const trimmed = source.trim();
  if (!trimmed) {
    return { points: null, error: "Points input is empty." };
  }

  const tokens = trimmed
    .split(/[\n;]+/)
    .map((line) => line.trim())
    .filter(Boolean);

  const points: GraphPoint[] = [];
  for (let index = 0; index < tokens.length; index += 1) {
    const line = tokens[index];
    const parts = line.split(/[\s,]+/).filter(Boolean);
    if (parts.length !== 2) {
      return { points: null, error: `Point ${index + 1} should be in \"x, y\" format.` };
    }
    const x = Number(parts[0]);
    const y = Number(parts[1]);
    if (!Number.isFinite(x) || !Number.isFinite(y)) {
      return { points: null, error: `Point ${index + 1} has an invalid number.` };
    }
    points.push({ x, y });
  }

  if (points.length === 0) {
    return { points: null, error: "At least one point is required." };
  }

  return { points, error: null };
}

export function buildGraphGeometry(specInput: GraphSpec, options: GraphBuildOptions): GraphGeometry {
  const spec = normaliseGraphSpec(specInput);
  const width = Math.max(64, Math.round(options.width));
  const height = Math.max(64, Math.round(options.height));
  const padding = options.padding == null ? 18 : Math.max(8, options.padding);
  const samples = options.samples == null ? DEFAULT_SAMPLES : Math.max(40, Math.round(options.samples));

  const plot: PlotRect = {
    x: padding,
    y: padding,
    width: Math.max(16, width - padding * 2),
    height: Math.max(16, height - padding * 2),
  };

  const validationError = validateGraphSpec(spec);
  if (validationError) {
    return {
      plot,
      xAxisY: null,
      yAxisX: null,
      segments: [],
      marks: [],
      error: validationError,
    };
  }

  const xAxisY = valueToScreenY(spec, plot, 0);
  const yAxisX = valueToScreenX(spec, plot, 0);

  if (spec.mode === "equation") {
    const equation = sanitizeEquation(spec.equation ?? "");
    let expression;
    try {
      expression = EQUATION_PARSER.parse(equation);
    } catch (err) {
      return {
        plot,
        xAxisY,
        yAxisX,
        segments: [],
        marks: [],
        error: `Equation error: ${toErrorMessage(err)}`,
      };
    }

    const segments: Array<Array<{ x: number; y: number }>> = [];
    let currentSegment: Array<{ x: number; y: number }> = [];

    for (let index = 0; index < samples; index += 1) {
      const ratio = samples <= 1 ? 0 : index / (samples - 1);
      const x = spec.xMin + ratio * (spec.xMax - spec.xMin);
      let y: number;
      try {
        y = Number(expression.evaluate({ x }));
      } catch {
        if (currentSegment.length > 1) segments.push(currentSegment);
        currentSegment = [];
        continue;
      }

      if (!Number.isFinite(y) || y < -1e9 || y > 1e9) {
        if (currentSegment.length > 1) segments.push(currentSegment);
        currentSegment = [];
        continue;
      }

      if (y < spec.yMin || y > spec.yMax) {
        if (currentSegment.length > 1) segments.push(currentSegment);
        currentSegment = [];
        continue;
      }

      currentSegment.push({
        x: valueToScreenX(spec, plot, x),
        y: valueToScreenY(spec, plot, y),
      });
    }

    if (currentSegment.length > 1) segments.push(currentSegment);

    return {
      plot,
      xAxisY,
      yAxisX,
      segments,
      marks: [],
      error: null,
    };
  }

  const sortedPoints = [...(spec.points ?? [])].sort((a, b) => a.x - b.x);
  const visiblePoints = sortedPoints.filter(
    (point) =>
      point.x >= spec.xMin
      && point.x <= spec.xMax
      && point.y >= spec.yMin
      && point.y <= spec.yMax,
  );

  const mappedPoints = visiblePoints.map((point) => ({
    x: valueToScreenX(spec, plot, point.x),
    y: valueToScreenY(spec, plot, point.y),
  }));

  return {
    plot,
    xAxisY,
    yAxisX,
    segments: mappedPoints.length > 1 ? [mappedPoints] : [],
    marks: mappedPoints,
    error: null,
  };
}

export function drawGraphCard(
  ctx: CanvasRenderingContext2D,
  specInput: GraphSpec,
  x: number,
  y: number,
  width: number,
  height: number,
  options: { selected?: boolean; showResizeHandle?: boolean } = {},
): void {
  const graphWidth = Math.max(24, width);
  const graphHeight = Math.max(24, height);
  const spec = normaliseGraphSpec(specInput);
  const geometry = buildGraphGeometry(spec, {
    width: graphWidth,
    height: graphHeight,
    padding: Math.max(12, Math.min(graphWidth, graphHeight) * 0.08),
  });

  ctx.save();
  ctx.translate(x, y);

  ctx.fillStyle = "rgba(255, 255, 255, 0.92)";
  ctx.fillRect(0, 0, graphWidth, graphHeight);

  ctx.lineWidth = options.selected ? 2 : 1;
  ctx.strokeStyle = options.selected ? "rgba(245, 130, 32, 0.95)" : "rgba(71, 85, 105, 0.48)";
  ctx.strokeRect(0.5, 0.5, graphWidth - 1, graphHeight - 1);

  const gridStepX = geometry.plot.width / 5;
  const gridStepY = geometry.plot.height / 5;
  ctx.strokeStyle = "rgba(148, 163, 184, 0.28)";
  ctx.lineWidth = 1;
  for (let i = 1; i < 5; i += 1) {
    const gx = geometry.plot.x + gridStepX * i;
    const gy = geometry.plot.y + gridStepY * i;
    ctx.beginPath();
    ctx.moveTo(gx, geometry.plot.y);
    ctx.lineTo(gx, geometry.plot.y + geometry.plot.height);
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(geometry.plot.x, gy);
    ctx.lineTo(geometry.plot.x + geometry.plot.width, gy);
    ctx.stroke();
  }

  ctx.strokeStyle = "rgba(71, 85, 105, 0.75)";
  ctx.lineWidth = 1.2;
  if (geometry.xAxisY != null) {
    ctx.beginPath();
    ctx.moveTo(geometry.plot.x, geometry.xAxisY);
    ctx.lineTo(geometry.plot.x + geometry.plot.width, geometry.xAxisY);
    ctx.stroke();
  }
  if (geometry.yAxisX != null) {
    ctx.beginPath();
    ctx.moveTo(geometry.yAxisX, geometry.plot.y);
    ctx.lineTo(geometry.yAxisX, geometry.plot.y + geometry.plot.height);
    ctx.stroke();
  }

  ctx.save();
  ctx.beginPath();
  ctx.rect(geometry.plot.x, geometry.plot.y, geometry.plot.width, geometry.plot.height);
  ctx.clip();

  ctx.strokeStyle = spec.lineColour;
  ctx.lineWidth = Math.max(1, spec.lineWidth);
  for (const segment of geometry.segments) {
    if (segment.length < 2) continue;
    ctx.beginPath();
    ctx.moveTo(segment[0].x, segment[0].y);
    for (let index = 1; index < segment.length; index += 1) {
      ctx.lineTo(segment[index].x, segment[index].y);
    }
    ctx.stroke();
  }

  if (geometry.marks.length > 0) {
    ctx.fillStyle = spec.lineColour;
    for (const mark of geometry.marks) {
      ctx.beginPath();
      ctx.arc(mark.x, mark.y, Math.max(2, spec.lineWidth * 0.9), 0, Math.PI * 2);
      ctx.fill();
    }
  }

  ctx.restore();

  if (geometry.error) {
    ctx.fillStyle = "rgba(153, 27, 27, 0.92)";
    ctx.font = "11px Inter, system-ui, sans-serif";
    ctx.textBaseline = "top";
    ctx.fillText("Graph error", 8, 8);
  }

  if (options.showResizeHandle) {
    const handleSize = 9;
    ctx.fillStyle = "rgba(37, 81, 209, 0.95)";
    ctx.fillRect(graphWidth - handleSize - 4, graphHeight - handleSize - 4, handleSize, handleSize);
  }

  ctx.restore();
}

export function graphSpecToFence(specInput: GraphSpec): string {
  const spec = normaliseGraphSpec(specInput);
  const payload: Record<string, unknown> = {
    mode: spec.mode,
    xMin: spec.xMin,
    xMax: spec.xMax,
    yMin: spec.yMin,
    yMax: spec.yMax,
    lineColour: spec.lineColour,
    lineWidth: spec.lineWidth,
  };

  if (spec.mode === "equation") {
    payload.equation = sanitizeEquation(spec.equation ?? "");
  } else {
    payload.points = spec.points ?? [];
  }

  return `\`\`\`graph\n${JSON.stringify(payload, null, 2)}\n\`\`\``;
}

export function parseGraphFence(source: string): { graph: GraphSpec | null; error: string | null } {
  const trimmed = source.trim();
  if (!trimmed) {
    return { graph: null, error: "Graph block is empty." };
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch (err) {
    return { graph: null, error: `Invalid JSON: ${toErrorMessage(err)}` };
  }

  if (!parsed || typeof parsed !== "object") {
    return { graph: null, error: "Graph block must be a JSON object." };
  }

  const obj = parsed as Record<string, unknown>;
  const modeValue = obj.mode;
  const mode: GraphMode = modeValue === "points" ? "points" : "equation";

  const pointCandidates = Array.isArray(obj.points)
    ? (obj.points as unknown[])
    : pointsFromJson(typeof obj.pointsJson === "string" ? obj.pointsJson : null);

  const points = Array.isArray(pointCandidates)
    ? pointCandidates
        .map((entry) => {
          if (!entry || typeof entry !== "object") return null;
          const x = finiteOrNull((entry as { x?: unknown }).x);
          const y = finiteOrNull((entry as { y?: unknown }).y);
          if (x == null || y == null) return null;
          return { x, y };
        })
        .filter((entry): entry is GraphPoint => entry != null)
    : null;

  const graph = normaliseGraphSpec({
    mode,
    equation: getString(obj.equation),
    points,
    xMin: finiteOrFallback(
      finiteOrNull(obj.xMin) ?? finiteOrNull(obj.x_min),
      DEFAULT_GRAPH_SPEC.xMin,
    ),
    xMax: finiteOrFallback(
      finiteOrNull(obj.xMax) ?? finiteOrNull(obj.x_max),
      DEFAULT_GRAPH_SPEC.xMax,
    ),
    yMin: finiteOrFallback(
      finiteOrNull(obj.yMin) ?? finiteOrNull(obj.y_min),
      DEFAULT_GRAPH_SPEC.yMin,
    ),
    yMax: finiteOrFallback(
      finiteOrNull(obj.yMax) ?? finiteOrNull(obj.y_max),
      DEFAULT_GRAPH_SPEC.yMax,
    ),
    lineColour: getString(obj.lineColour) ?? getString(obj.line_colour) ?? DEFAULT_GRAPH_COLOUR,
    lineWidth: finiteOrFallback(
      finiteOrNull(obj.lineWidth) ?? finiteOrNull(obj.line_width),
      DEFAULT_GRAPH_SPEC.lineWidth,
    ),
  });

  const error = validateGraphSpec(graph);
  if (error) return { graph: null, error };
  return { graph, error: null };
}

export function renderGraphSvg(specInput: GraphSpec, width: number, height: number): string {
  const spec = normaliseGraphSpec(specInput);
  const geometry = buildGraphGeometry(spec, { width, height, padding: 16, samples: 260 });

  const segments = geometry.segments
    .filter((segment) => segment.length > 1)
    .map((segment) => {
      const path = segment
        .map((point, index) => `${index === 0 ? "M" : "L"}${point.x.toFixed(2)} ${point.y.toFixed(2)}`)
        .join(" ");
      return `<path d="${path}" fill="none" stroke="${escapeHtmlAttribute(spec.lineColour)}" stroke-width="${spec.lineWidth.toFixed(2)}" stroke-linecap="round" stroke-linejoin="round" />`;
    })
    .join("");

  const marks = geometry.marks
    .map((point) => `<circle cx="${point.x.toFixed(2)}" cy="${point.y.toFixed(2)}" r="${Math.max(2, spec.lineWidth * 0.85).toFixed(2)}" fill="${escapeHtmlAttribute(spec.lineColour)}" />`)
    .join("");

  const xAxis = geometry.xAxisY == null
    ? ""
    : `<line x1="${geometry.plot.x.toFixed(2)}" y1="${geometry.xAxisY.toFixed(2)}" x2="${(geometry.plot.x + geometry.plot.width).toFixed(2)}" y2="${geometry.xAxisY.toFixed(2)}" stroke="rgba(71,85,105,0.75)" stroke-width="1.1" />`;
  const yAxis = geometry.yAxisX == null
    ? ""
    : `<line x1="${geometry.yAxisX.toFixed(2)}" y1="${geometry.plot.y.toFixed(2)}" x2="${geometry.yAxisX.toFixed(2)}" y2="${(geometry.plot.y + geometry.plot.height).toFixed(2)}" stroke="rgba(71,85,105,0.75)" stroke-width="1.1" />`;

  const grid = buildSvgGrid(geometry.plot);

  const label = spec.mode === "equation"
    ? sanitizeEquation(spec.equation ?? "")
    : `${spec.points?.length ?? 0} point${(spec.points?.length ?? 0) === 1 ? "" : "s"}`;

  const error = geometry.error
    ? `<text x="10" y="18" font-size="11" fill="rgba(153,27,27,0.95)">Graph error</text>`
    : "";

  return `
<figure class="chunk-graph-block">
  <svg viewBox="0 0 ${Math.round(width)} ${Math.round(height)}" role="img" aria-label="Graph preview" preserveAspectRatio="none">
    <rect x="0" y="0" width="${Math.round(width)}" height="${Math.round(height)}" fill="rgba(255,255,255,0.96)" stroke="rgba(71,85,105,0.45)" stroke-width="1" />
    ${grid}
    ${xAxis}
    ${yAxis}
    ${segments}
    ${marks}
    ${error}
  </svg>
  <figcaption>${escapeHtml(label || "Graph")}</figcaption>
</figure>`.trim();
}

export function renderGraphFenceHtml(source: string): string {
  const parsed = parseGraphFence(source);
  if (!parsed.graph) {
    return `<div class="chunk-graph-error">Invalid graph block: ${escapeHtml(parsed.error ?? "Unknown error")}</div>`;
  }
  return renderGraphSvg(parsed.graph, 460, 300);
}

function buildSvgGrid(plot: PlotRect): string {
  const lines: string[] = [];
  for (let index = 1; index < 5; index += 1) {
    const x = plot.x + (plot.width / 5) * index;
    const y = plot.y + (plot.height / 5) * index;
    lines.push(
      `<line x1="${x.toFixed(2)}" y1="${plot.y.toFixed(2)}" x2="${x.toFixed(2)}" y2="${(plot.y + plot.height).toFixed(2)}" stroke="rgba(148,163,184,0.28)" stroke-width="1" />`,
    );
    lines.push(
      `<line x1="${plot.x.toFixed(2)}" y1="${y.toFixed(2)}" x2="${(plot.x + plot.width).toFixed(2)}" y2="${y.toFixed(2)}" stroke="rgba(148,163,184,0.28)" stroke-width="1" />`,
    );
  }
  return lines.join("");
}

function valueToScreenX(spec: GraphSpec, plot: PlotRect, value: number): number {
  if (value < spec.xMin || value > spec.xMax) return clamp((value - spec.xMin) / (spec.xMax - spec.xMin), 0, 1) * plot.width + plot.x;
  return ((value - spec.xMin) / (spec.xMax - spec.xMin)) * plot.width + plot.x;
}

function valueToScreenY(spec: GraphSpec, plot: PlotRect, value: number): number {
  if (value < spec.yMin || value > spec.yMax) return plot.y + (1 - clamp((value - spec.yMin) / (spec.yMax - spec.yMin), 0, 1)) * plot.height;
  return plot.y + (1 - (value - spec.yMin) / (spec.yMax - spec.yMin)) * plot.height;
}

function sanitizeEquation(raw: string): string {
  const trimmed = raw.trim().replaceAll("−", "-");
  if (!trimmed) return "";
  const lowered = trimmed.toLowerCase();
  if (lowered.startsWith("y=")) return trimmed.slice(2).trim();
  if (lowered.startsWith("y =")) return trimmed.slice(3).trim();
  return trimmed;
}

function sanitizeColour(raw: string | null | undefined): string {
  const trimmed = raw?.trim();
  if (!trimmed) return DEFAULT_GRAPH_COLOUR;
  return trimmed;
}

function finiteOrFallback(value: unknown, fallback: number): number {
  const numeric = typeof value === "number" ? value : Number(value);
  return Number.isFinite(numeric) ? numeric : fallback;
}

function finiteOrNull(value: unknown): number | null {
  const numeric = typeof value === "number" ? value : Number(value);
  return Number.isFinite(numeric) ? numeric : null;
}

function getString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function toErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return String(error);
}

function clamp(value: number, min: number, max: number): number {
  return Math.max(min, Math.min(max, value));
}

function trimNumber(value: number): string {
  if (Number.isInteger(value)) return `${value}`;
  return value.toFixed(4).replace(/\.0+$/, "").replace(/(\.\d*?)0+$/, "$1");
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
