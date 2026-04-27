// Shared ink types, constants, and rendering utilities used by both the main
// page canvas and the chunk surface canvas.

export type Point = { x: number; y: number; pressure: number };
export type BBox   = { minX: number; minY: number; maxX: number; maxY: number };
export type Stroke = {
  id: number | null;
  colour: string;
  thickness: number;
  points: Point[];
  bbox: BBox;
  chunkId: number | null;
  groupId?: string | null;
};
export type Rect = { x: number; y: number; w: number; h: number };
export type ShapeKind = "circle" | "graph_full" | "graph_positive";

export const PRESSURE_WIDTH_THRESHOLD = 0.5;
export const DEFAULT_PEN_COLOUR    = "#2351d1";
export const DEFAULT_PEN_THICKNESS = 3;

export function widthForPoint(thickness: number, pressure: number, scale: number): number {
  return (thickness * (0.5 + pressure)) / scale;
}

export function computeBBox(points: Point[]): BBox {
  let minX = points[0].x, minY = points[0].y;
  let maxX = minX, maxY = minY;
  for (const p of points) {
    if (p.x < minX) minX = p.x;
    if (p.y < minY) minY = p.y;
    if (p.x > maxX) maxX = p.x;
    if (p.y > maxY) maxY = p.y;
  }
  return { minX, minY, maxX, maxY };
}

function shapePoint(x: number, y: number, pressure: number): Point {
  return { x, y, pressure };
}

export function shapeDragLengthWorld(
  origin: Pick<Point, "x" | "y">,
  current: Pick<Point, "x" | "y">,
  surfaceW: number,
  surfaceH: number,
): number {
  const dx = (current.x - origin.x) * surfaceW;
  const dy = (current.y - origin.y) * surfaceH;
  return Math.hypot(dx, dy);
}

function buildCircleStroke(
  origin: Pick<Point, "x" | "y">,
  radiusWorld: number,
  surfaceW: number,
  surfaceH: number,
  pressure: number,
): Point[] {
  const radiusNormX = radiusWorld / surfaceW;
  const radiusNormY = radiusWorld / surfaceH;
  const circumference = 2 * Math.PI * radiusWorld;
  const segmentCount = Math.max(24, Math.min(120, Math.ceil(circumference / 14)));
  const points: Point[] = [];
  for (let i = 0; i <= segmentCount; i++) {
    const t = (i / segmentCount) * Math.PI * 2;
    points.push(
      shapePoint(
        origin.x + Math.cos(t) * radiusNormX,
        origin.y + Math.sin(t) * radiusNormY,
        pressure,
      ),
    );
  }
  return points;
}

function buildFullGraphStrokes(
  origin: Pick<Point, "x" | "y">,
  armLengthWorld: number,
  surfaceW: number,
  surfaceH: number,
  pressure: number,
): Point[][] {
  const armNormX = armLengthWorld / surfaceW;
  const armNormY = armLengthWorld / surfaceH;
  return [
    [
      shapePoint(origin.x - armNormX, origin.y, pressure),
      shapePoint(origin.x + armNormX, origin.y, pressure),
    ],
    [
      shapePoint(origin.x, origin.y - armNormY, pressure),
      shapePoint(origin.x, origin.y + armNormY, pressure),
    ],
  ];
}

function buildPositiveGraphStrokes(
  origin: Pick<Point, "x" | "y">,
  armLengthWorld: number,
  surfaceW: number,
  surfaceH: number,
  pressure: number,
): Point[][] {
  const armNormX = armLengthWorld / surfaceW;
  const armNormY = armLengthWorld / surfaceH;
  return [
    [
      shapePoint(origin.x, origin.y, pressure),
      shapePoint(origin.x + armNormX, origin.y, pressure),
    ],
    [
      shapePoint(origin.x, origin.y, pressure),
      shapePoint(origin.x, origin.y - armNormY, pressure),
    ],
  ];
}

export function buildShapeStrokes(
  kind: ShapeKind,
  origin: Pick<Point, "x" | "y">,
  current: Pick<Point, "x" | "y">,
  surfaceW: number,
  surfaceH: number,
  pressure = 0.5,
): Point[][] {
  if (surfaceW <= 0 || surfaceH <= 0) return [];
  const dragLengthWorld = shapeDragLengthWorld(origin, current, surfaceW, surfaceH);
  if (dragLengthWorld <= 0) return [];
  if (kind === "circle") {
    return [buildCircleStroke(origin, dragLengthWorld, surfaceW, surfaceH, pressure)];
  }
  if (kind === "graph_full") {
    return buildFullGraphStrokes(origin, dragLengthWorld, surfaceW, surfaceH, pressure);
  }
  return buildPositiveGraphStrokes(origin, dragLengthWorld, surfaceW, surfaceH, pressure);
}

/**
 * Draw stroke points onto a canvas context.
 *
 * Coordinates are in normalised surface space [0, 1] and are transformed to
 * world space via: world = origin + norm * size.
 *
 * For the main page canvas: originX = pageOrigin.x, sizeW = pageSize.w, etc.
 * For the chunk canvas:     originX = 0, sizeW = chunkSurfaceSize.w, etc.
 *
 * Uses midpoint quadratic Bézier curves for smooth rendering. Splits the path
 * when pressure-based line width changes significantly. No intermediate array
 * allocations — iterates pts directly from startIdx.
 */
export function drawStrokePoints(
  ctx: CanvasRenderingContext2D,
  pts: Point[],
  colour: string,
  thickness: number,
  startIdx: number,
  scale: number,
  originX: number,
  originY: number,
  sizeW: number,
  sizeH: number,
): void {
  const len = pts.length;
  if (len - startIdx < 2) return;

  ctx.strokeStyle = colour;
  ctx.beginPath();

  const p0 = pts[startIdx];
  ctx.moveTo(originX + p0.x * sizeW, originY + p0.y * sizeH);
  ctx.lineWidth = widthForPoint(thickness, p0.pressure, scale);

  for (let i = startIdx + 1; i < len - 1; i++) {
    const pi = pts[i];
    const pn = pts[i + 1];
    const wxi = originX + pi.x * sizeW;
    const wyi = originY + pi.y * sizeH;
    const wxn = originX + pn.x * sizeW;
    const wyn = originY + pn.y * sizeH;
    const width = widthForPoint(thickness, pi.pressure, scale);
    if (Math.abs(ctx.lineWidth - width) > PRESSURE_WIDTH_THRESHOLD / scale) {
      ctx.stroke();
      ctx.beginPath();
      const pp = pts[i - 1];
      ctx.moveTo(
        (originX + pp.x * sizeW + wxi) / 2,
        (originY + pp.y * sizeH + wyi) / 2,
      );
      ctx.lineWidth = width;
    }
    ctx.quadraticCurveTo(wxi, wyi, (wxi + wxn) / 2, (wyi + wyn) / 2);
  }

  const last = pts[len - 1];
  const prev = pts[len - 2];
  ctx.quadraticCurveTo(
    originX + prev.x * sizeW,
    originY + prev.y * sizeH,
    originX + last.x * sizeW,
    originY + last.y * sizeH,
  );
  ctx.stroke();
}
