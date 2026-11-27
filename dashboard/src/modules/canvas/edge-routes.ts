/** A rectangle in canvas coordinates. */
export type Box = { x: number; y: number; width: number; height: number };
export type Point = { x: number; y: number };
export type RouteEdge = { id: string; source: string; target: string };
/** How a line is drawn: its SVG path, where its label sits, and the corners it turns at. */
export type EdgeRoute = { path: string; labelAt: Point; points: Point[] };

type Side = "top" | "right" | "bottom" | "left";

/** How far a line keeps from every card and overlay. */
const CLEARANCE = 20;
/** What a turn costs, in pixels of length: fewer turns beat a slightly shorter line. */
const BEND_COST = 48;
/** How much longer a stretch already taken by another line counts, so lines take lanes of their own. */
const SHARED_COST = 2;
/** How much longer a stretch inside another card's clearance counts: allowed where cards crowd, avoided elsewhere. */
const CROWDED_COST = 6;
const PORT_SPACING = 24;
const LANE_SPACING = 6;
const CORNER_RADIUS = 8;

const NORMAL = {
  top: { x: 0, y: -1 }, right: { x: 1, y: 0 }, bottom: { x: 0, y: 1 }, left: { x: -1, y: 0 },
} satisfies Record<Side, Point>;
/** Grid step directions by index: +x, -x, +y, -y; `direction ^ 1` is its reverse. */
const STEPS = [{ x: 1, y: 0 }, { x: -1, y: 0 }, { x: 0, y: 1 }, { x: 0, y: -1 }] as const;
const directionOf = ({ x, y }: Point) => x > 0 ? 0 : x < 0 ? 1 : y > 0 ? 2 : 3;

const centre = (box: Box): Point => ({ x: box.x + box.width / 2, y: box.y + box.height / 2 });
const grow = (box: Box, by: number): Box => ({ x: box.x - by, y: box.y - by, width: box.width + 2 * by, height: box.height + 2 * by });
const strictlyInside = (point: Point, box: Box) =>
  point.x > box.x && point.x < box.x + box.width && point.y > box.y && point.y < box.y + box.height;
const halfPixel = (value: number) => Math.round(value * 2) / 2;
const distance = (a: Point, b: Point) => Math.hypot(b.x - a.x, b.y - a.y);

/** Each neighbouring pair of `items`, in order. */
function pairs<T>(items: readonly T[]): [T, T][] {
  const out: [T, T][] = [];
  let before: T | undefined;
  for (const item of items) {
    if (before !== undefined) out.push([before, item]);
    before = item;
  }
  return out;
}

/** The sides a line leaves `source` and enters `target` by: the pair facing each other across the wider gap. */
function facingSides(source: Box, target: Box): [Side, Side] {
  const gapX = Math.max(target.x - (source.x + source.width), source.x - (target.x + target.width));
  const gapY = Math.max(target.y - (source.y + source.height), source.y - (target.y + target.height));
  const from = centre(source);
  const to = centre(target);
  if (gapX >= gapY) return to.x >= from.x ? ["right", "left"] : ["left", "right"];
  return to.y >= from.y ? ["bottom", "top"] : ["top", "bottom"];
}

type End = { key: string; box: Box; side: Side; toward: Point };
type Port = { at: Point; side: Side; stub: Point };

/** Each line end's port, spread along a side several share in the order of where they go, so they don't cross. */
function ports(ends: End[]) {
  const bySide = new Map<string, { side: Side; box: Box; ends: End[] }>();
  for (const end of ends) {
    const key = `${end.box.x},${end.box.y},${end.box.width},${end.box.height},${end.side}`;
    const sharing = bySide.get(key) ?? { side: end.side, box: end.box, ends: [] };
    sharing.ends.push(end);
    bySide.set(key, sharing);
  }
  const placed = new Map<string, Port>();
  for (const { side, box, ends: sharing } of bySide.values()) {
    const horizontal = side === "top" || side === "bottom";
    const along = (point: Point) => horizontal ? point.x : point.y;
    sharing.sort((a, b) => along(a.toward) - along(b.toward) || a.key.localeCompare(b.key));
    const spacing = Math.min(PORT_SPACING, (horizontal ? box.width : box.height) / (sharing.length + 1));
    sharing.forEach((end, index) => {
      // On the grid's half-pixel lines along the side, so the line leaves straight; exactly on the side across it.
      const offset = halfPixel(along(centre(box)) + (index - (sharing.length - 1) / 2) * spacing);
      const at = horizontal
        ? { x: offset, y: side === "top" ? box.y : box.y + box.height }
        : { x: side === "left" ? box.x : box.x + box.width, y: offset };
      const normal = NORMAL[side];
      placed.set(end.key, { at, side, stub: { x: at.x + normal.x * CLEARANCE, y: at.y + normal.y * CLEARANCE } });
    });
  }
  return placed;
}

/** Coordinates the grid runs along: obstacle edges, line stubs, the middles between them, and a margin outside all. */
function axis(values: number[]): number[] {
  const sorted = [...new Set(values.map(halfPixel))].sort((a, b) => a - b);
  const lines = [Math.min(...sorted) - CLEARANCE * 2, ...sorted, Math.max(...sorted) + CLEARANCE * 2];
  const middles = pairs(lines).map(([a, b]) => (a + b) / 2);
  return [...new Set([...lines, ...middles])].sort((a, b) => a - b);
}

/** A binary min-heap of grid states by cost. */
class Heap {
  private readonly keys: number[] = [];
  private readonly costs: number[] = [];
  get size() { return this.keys.length; }
  private swap(a: number, b: number) {
    [this.keys[a], this.keys[b]] = [this.keys[b] ?? 0, this.keys[a] ?? 0];
    [this.costs[a], this.costs[b]] = [this.costs[b] ?? 0, this.costs[a] ?? 0];
  }
  private cost(index: number) { return this.costs[index] ?? Infinity; }
  push(key: number, cost: number) {
    this.keys.push(key);
    this.costs.push(cost);
    let index = this.keys.length - 1;
    while (index > 0) {
      const parent = (index - 1) >> 1;
      if (this.cost(parent) <= this.cost(index)) break;
      this.swap(parent, index);
      index = parent;
    }
  }
  pop() {
    const top = { key: this.keys[0] ?? 0, cost: this.costs[0] ?? Infinity };
    this.swap(0, this.keys.length - 1);
    this.keys.pop();
    this.costs.pop();
    let index = 0;
    for (;;) {
      const left = index * 2 + 1;
      let least = index;
      if (this.cost(left) < this.cost(least)) least = left;
      if (this.cost(left + 1) < this.cost(least)) least = left + 1;
      if (least === index) return top;
      this.swap(least, index);
      index = least;
    }
  }
}

/** Drops corners where a line runs straight on, and repeated points. */
function straighten(points: Point[]): Point[] {
  const kept: Point[] = [];
  for (const point of points) {
    const last = kept.at(-1);
    if (last && last.x === point.x && last.y === point.y) continue;
    const before = kept.at(-2);
    if (before && last && ((before.x === last.x && last.x === point.x) || (before.y === last.y && last.y === point.y))) kept.pop();
    kept.push(point);
  }
  return kept;
}

/**
 * Lines that share a stretch move apart into lanes. The stretch at each end stays, so a line still meets its card at its
 * port; the others shift sideways by a few pixels, well inside the clearance.
 */
function spreadLanes(routes: Point[][]) {
  type Stretch = { route: Point[]; order: number; index: number; fixed: boolean; from: number; to: number };
  const groups = new Map<string, { vertical: boolean; stretches: Stretch[] }>();
  routes.forEach((route, order) => {
    const last = route.length - 2;
    pairs(route).forEach(([a, b], index) => {
      const vertical = a.x === b.x;
      const key = `${vertical ? "v" : "h"}${vertical ? a.x : a.y}`;
      const group = groups.get(key) ?? { vertical, stretches: [] };
      group.stretches.push({
        route, order, index, fixed: index === 0 || index === last,
        from: vertical ? Math.min(a.y, b.y) : Math.min(a.x, b.x), to: vertical ? Math.max(a.y, b.y) : Math.max(a.x, b.x),
      });
      groups.set(key, group);
    });
  });
  for (const { vertical, stretches } of groups.values()) {
    stretches.sort((a, b) => a.from - b.from);
    const clusters: Stretch[][] = [];
    let reach = -Infinity;
    for (const stretch of stretches) {
      const overlapping = stretch.from < reach ? clusters.at(-1) : undefined;
      if (overlapping) overlapping.push(stretch);
      else clusters.push([stretch]);
      reach = overlapping ? Math.max(reach, stretch.to) : stretch.to;
    }
    for (const cluster of clusters) {
      if (new Set(cluster.map(({ route }) => route)).size < 2) continue;
      const movable = cluster.filter(({ fixed }) => !fixed).sort((a, b) => a.order - b.order);
      const anyFixed = movable.length < cluster.length;
      const lanes = movable.map((_, index) => anyFixed
        ? (index % 2 === 0 ? 1 : -1) * (Math.floor(index / 2) + 1)
        : index - (movable.length - 1) / 2);
      const spacing = Math.min(LANE_SPACING, CLEARANCE / 2 / Math.max(1, ...lanes.map(Math.abs)));
      movable.forEach(({ route, index }, lane) => {
        const shift = (lanes[lane] ?? 0) * spacing;
        for (const at of [index, index + 1]) {
          const point = route[at];
          if (point) route[at] = vertical ? { x: point.x + shift, y: point.y } : { x: point.x, y: point.y + shift };
        }
      });
    }
  }
}

/** The line through `points` as an SVG path, each turn rounded. */
function roundedPath(points: Point[]): string {
  const [first] = points;
  const last = points.at(-1);
  if (!first || !last) return "";
  const turns = pairs(pairs(points)).map(([[before, at], [, after]]) => {
    const radius = Math.min(CORNER_RADIUS, distance(before, at) / 2, distance(at, after) / 2);
    const toward = (point: Point) => {
      const length = distance(at, point) || 1;
      return `${at.x + ((point.x - at.x) / length) * radius},${at.y + ((point.y - at.y) / length) * radius}`;
    };
    return `L${toward(before)} Q${at.x},${at.y} ${toward(after)}`;
  });
  return [`M${first.x},${first.y}`, ...turns, `L${last.x},${last.y}`].join(" ");
}

/** Halfway along the line. */
function midpoint(points: Point[]): Point {
  const stretches = pairs(points);
  let left = stretches.reduce((sum, [a, b]) => sum + distance(a, b), 0) / 2;
  for (const [a, b] of stretches) {
    const length = distance(a, b);
    if (left <= length) {
      const share = length ? left / length : 0;
      return { x: a.x + (b.x - a.x) * share, y: a.y + (b.y - a.y) * share };
    }
    left -= length;
  }
  return points[0] ?? { x: 0, y: 0 };
}

/**
 * Routes each line between its cards: it leaves the side of its source that faces its target and enters the side of
 * its target that faces back, turns only at right angles with rounded corners, and keeps clear of every card and every
 * `keepOut` rectangle (floating bars over the canvas). Lines sharing a side spread along it, and lines sharing a
 * stretch take lanes of their own. A line whose card isn't in `boxes`, or that links a card to itself, gets no route.
 */
export function routeEdges({ boxes, edges, keepOut = [] }: {
  boxes: ReadonlyMap<string, Box>; edges: readonly RouteEdge[]; keepOut?: readonly Box[];
}): Map<string, EdgeRoute> {
  const drawn = edges.flatMap((edge) => {
    const source = boxes.get(edge.source);
    const target = boxes.get(edge.target);
    return source && target && edge.source !== edge.target ? [{ id: edge.id, source, target }] : [];
  });
  if (!drawn.length) return new Map();
  const portOf = ports(drawn.flatMap(({ id, source, target }): End[] => {
    const [from, to] = facingSides(source, target);
    return [{ key: `${id}:out`, box: source, side: from, toward: centre(target) },
      { key: `${id}:in`, box: target, side: to, toward: centre(source) }];
  }));
  const lines = drawn.flatMap(({ id }) => {
    const out = portOf.get(`${id}:out`);
    const into = portOf.get(`${id}:in`);
    return out && into ? [{ id, out, into }] : [];
  }).sort((a, b) => {
    const span = ({ out, into }: typeof a) => Math.abs(out.at.x - into.at.x) + Math.abs(out.at.y - into.at.y);
    return span(a) - span(b) || a.id.localeCompare(b.id);
  });

  const solid = [...boxes.values(), ...keepOut];
  const margins = solid.map((box) => grow(box, CLEARANCE));
  const stubs = lines.flatMap(({ out, into }) => [out.stub, into.stub]);
  const outlines = [...solid, ...margins];
  const xs = axis([...outlines.flatMap((box) => [box.x, box.x + box.width]), ...stubs.map(({ x }) => x)]);
  const ys = axis([...outlines.flatMap((box) => [box.y, box.y + box.height]), ...stubs.map(({ y }) => y)]);
  const columnOf = new Map(xs.map((x, index) => [x, index]));
  const rowOf = new Map(ys.map((y, index) => [y, index]));
  const rows = ys.length;
  const nodes = xs.length * rows;
  const pointOf = (node: number): Point => ({ x: xs[Math.floor(node / rows)] ?? 0, y: ys[node % rows] ?? 0 });
  const nodeAt = (point: Point) => {
    const column = columnOf.get(halfPixel(point.x));
    const row = rowOf.get(halfPixel(point.y));
    return column === undefined || row === undefined ? undefined : column * rows + row;
  };
  // The step from a node to its neighbour along +x (0) or +y (1) is blocked (2) when its middle is inside a card or bar,
  // and crowded (1) when it is inside one's clearance. Box and clearance edges are grid lines, so none fits between two
  // neighbours.
  const terrain = [new Uint8Array(nodes), new Uint8Array(nodes)] as const;
  const taken = [new Uint8Array(nodes), new Uint8Array(nodes)] as const;
  const terrainAt = (middle: Point) =>
    solid.some((box) => strictlyInside(middle, box)) ? 2 : margins.some((box) => strictlyInside(middle, box)) ? 1 : 0;
  for (let node = 0; node < nodes; node++) {
    const point = pointOf(node);
    const right = xs[Math.floor(node / rows) + 1];
    const below = node % rows + 1 < rows ? ys[node % rows + 1] : undefined;
    if (right !== undefined) terrain[0][node] = terrainAt({ x: (point.x + right) / 2, y: point.y });
    if (below !== undefined) terrain[1][node] = terrainAt({ x: point.x, y: (point.y + below) / 2 });
  }

  /** The cheapest grid walk from `start` heading `heading` to `goal`, arriving heading `arrive`: length, turns and shared stretches. */
  const walk = (start: number, heading: number, goal: number, arrive: number): number[] | null => {
    const best = new Float64Array(nodes * 4).fill(Infinity);
    const cameFrom = new Int32Array(nodes * 4).fill(-1);
    const heap = new Heap();
    const target = pointOf(goal);
    const guess = (node: number) => {
      const point = pointOf(node);
      return Math.abs(point.x - target.x) + Math.abs(point.y - target.y);
    };
    best[start * 4 + heading] = 0;
    heap.push(start * 4 + heading, guess(start));
    let finish = -1;
    let finishCost = Infinity;
    while (heap.size) {
      const { key, cost } = heap.pop();
      if (cost >= finishCost) break;
      const node = Math.floor(key / 4);
      const direction = key % 4;
      const spent = best[key] ?? Infinity;
      if (spent + guess(node) < cost - 1e-6) continue;
      if (node === goal) {
        const total = spent + (direction === arrive ? 0 : BEND_COST);
        if (total < finishCost) {
          finishCost = total;
          finish = key;
        }
        continue;
      }
      const column = Math.floor(node / rows);
      const row = node % rows;
      STEPS.forEach((step, next) => {
        if ((next ^ 1) === direction) return;
        const nextColumn = column + step.x;
        const nextRow = row + step.y;
        if (nextColumn < 0 || nextColumn >= xs.length || nextRow < 0 || nextRow >= rows) return;
        const neighbour = nextColumn * rows + nextRow;
        const along = step.x === 0 ? 1 : 0;
        const stepFrom = Math.min(node, neighbour);
        const ground = terrain[along][stepFrom] ?? 2;
        if (ground === 2) return;
        const length = distance(pointOf(node), pointOf(neighbour));
        const stepCost = length * (taken[along][stepFrom] ? SHARED_COST : 1) * (ground ? CROWDED_COST : 1)
          + (next === direction ? 0 : BEND_COST);
        const nextKey = neighbour * 4 + next;
        if (spent + stepCost < (best[nextKey] ?? Infinity)) {
          best[nextKey] = spent + stepCost;
          cameFrom[nextKey] = key;
          heap.push(nextKey, spent + stepCost + guess(neighbour));
        }
      });
    }
    if (finish < 0) return null;
    const path: number[] = [];
    for (let key = finish; key >= 0; key = cameFrom[key] ?? -1) path.push(Math.floor(key / 4));
    return path.reverse();
  };

  const routed = lines.map(({ out, into }) => {
    const outward = directionOf(NORMAL[out.side]);
    const inward = directionOf(NORMAL[into.side]) ^ 1;
    const start = nodeAt(out.stub);
    const goal = nodeAt(into.stub);
    const grid = start === undefined || goal === undefined ? null : walk(start, outward, goal, inward);
    if (!grid) {
      // Boxed in on every side: a plain elbow between the stubs, which a later layout will route properly.
      const sideways = outward < 2;
      const elbow = sideways ? { x: (out.stub.x + into.stub.x) / 2, y: out.stub.y } : { x: out.stub.x, y: (out.stub.y + into.stub.y) / 2 };
      const back = sideways ? { x: elbow.x, y: into.stub.y } : { x: into.stub.x, y: elbow.y };
      return straighten([out.at, out.stub, elbow, back, into.stub, into.at]);
    }
    for (const [a, b] of pairs(grid)) taken[Math.floor(a / rows) === Math.floor(b / rows) ? 1 : 0][Math.min(a, b)] = 1;
    return straighten([out.at, ...grid.map(pointOf), into.at]);
  });
  spreadLanes(routed);
  return new Map(lines.map(({ id }, index) => {
    const points = routed[index] ?? [];
    return [id, { path: roundedPath(points), labelAt: midpoint(points), points }];
  }));
}
