import { describe, expect, it } from "vitest";
import { routeEdges, type Box, type EdgeRoute, type Point, type RouteEdge } from "./edge-routes";

/** `value`, failing the test when it is missing. */
function must<T>(value: T | undefined): T {
  if (value === undefined) throw new Error("missing");
  return value;
}

const box = (x: number, y: number, width = 224, height = 72): Box => ({ x, y, width, height });

/** Whether the stretch from `a` to `b` passes through the inside of `rect`, not only along or onto its edge. */
function crosses(a: Point, b: Point, rect: Box) {
  const [left, right] = [Math.min(a.x, b.x), Math.max(a.x, b.x)];
  const [top, bottom] = [Math.min(a.y, b.y), Math.max(a.y, b.y)];
  return left < rect.x + rect.width && right > rect.x && top < rect.y + rect.height && bottom > rect.y
    && !(a.x === b.x && (a.x <= rect.x || a.x >= rect.x + rect.width))
    && !(a.y === b.y && (a.y <= rect.y || a.y >= rect.y + rect.height));
}

const stretches = (route: EdgeRoute) => route.points.slice(1).flatMap((point, index) => {
  const before = route.points[index];
  return before ? [[before, point] as const] : [];
});

/** Which side of `rect` the point lies on, if any. */
function sideOf(point: Point, rect: Box) {
  if (point.y === rect.y && point.x >= rect.x && point.x <= rect.x + rect.width) return "top";
  if (point.y === rect.y + rect.height && point.x >= rect.x && point.x <= rect.x + rect.width) return "bottom";
  if (point.x === rect.x && point.y >= rect.y && point.y <= rect.y + rect.height) return "left";
  if (point.x === rect.x + rect.width && point.y >= rect.y && point.y <= rect.y + rect.height) return "right";
  return null;
}

/** The side of `from` that faces `to` across the wider gap. */
function facing(from: Box, to: Box) {
  const gapX = Math.max(to.x - (from.x + from.width), from.x - (to.x + to.width));
  const gapY = Math.max(to.y - (from.y + from.height), from.y - (to.y + to.height));
  if (gapX >= gapY) return to.x + to.width / 2 >= from.x + from.width / 2 ? "right" : "left";
  return to.y + to.height / 2 >= from.y + from.height / 2 ? "bottom" : "top";
}

/** Every route keeps out of every card and keep-out rectangle, meets its cards on their facing sides, and shares no stretch. */
function expectWellRouted(boxes: Map<string, Box>, edges: RouteEdge[], keepOut: Box[] = []) {
  const routes = routeEdges({ boxes, edges, keepOut });
  expect([...routes.keys()].sort()).toEqual(edges.map(({ id }) => id).sort());
  for (const edge of edges) {
    const route = must(routes.get(edge.id));
    const [source, target] = [must(boxes.get(edge.source)), must(boxes.get(edge.target))];
    for (const [a, b] of stretches(route)) {
      expect(a.x === b.x || a.y === b.y, `${edge.id} turns only at right angles`).toBe(true);
      for (const [id, rect] of boxes) expect(crosses(a, b, rect), `${edge.id} passes through ${id}`).toBe(false);
      for (const rect of keepOut) expect(crosses(a, b, rect), `${edge.id} passes under a keep-out`).toBe(false);
    }
    expect(sideOf(must(route.points[0]), source), `${edge.id} leaves its source's facing side`).toBe(facing(source, target));
    expect(sideOf(must(route.points[route.points.length - 1]), target), `${edge.id} enters its target's facing side`).toBe(facing(target, source));
  }
  const all = edges.flatMap(({ id }) => stretches(must(routes.get(id))).map(([a, b]) => ({ id, a, b })));
  for (const [index, one] of all.entries()) {
    for (const other of all.slice(index + 1)) {
      if (one.id === other.id) continue;
      const vertical = one.a.x === one.b.x && other.a.x === other.b.x && one.a.x === other.a.x;
      const horizontal = one.a.y === one.b.y && other.a.y === other.b.y && one.a.y === other.a.y;
      if (!vertical && !horizontal) continue;
      const span = (s: typeof one) => vertical
        ? { from: Math.min(s.a.y, s.b.y), to: Math.max(s.a.y, s.b.y) }
        : { from: Math.min(s.a.x, s.b.x), to: Math.max(s.a.x, s.b.x) };
      const [a, b] = [span(one), span(other)];
      expect(Math.min(a.to, b.to) - Math.max(a.from, b.from) <= 0, `${one.id} and ${other.id} share a stretch`).toBe(true);
    }
  }
  return routes;
}

/** A small seeded generator, so a failing layout reproduces. */
function random(seed: number) {
  let state = seed;
  return () => {
    state = (state * 1664525 + 1013904223) % 2 ** 32;
    return state / 2 ** 32;
  };
}

describe("routeEdges", () => {
  it("takes a line that skips a column around the card between, not under it", () => {
    // #2133: db -> api -> web on one row, plus db -> web.
    const boxes = new Map([["db", box(0, 0)], ["api", box(272, 0)], ["web", box(544, 0)]]);
    const routes = expectWellRouted(boxes, [
      { id: "db-api", source: "db", target: "api" },
      { id: "api-web", source: "api", target: "web" },
      { id: "db-web", source: "db", target: "web" },
    ]);
    const api = must(boxes.get("api"));
    expect(must(routes.get("db-web")).points.some(({ y }) => y < api.y || y > api.y + api.height)).toBe(true);
  });

  it("goes around a floating bar rather than under it", () => {
    const boxes = new Map([["redis", box(0, 0, 368, 184)], ["web", box(-200, 560, 368, 232)]]);
    const bar = { x: -100, y: 300, width: 340, height: 60 };
    expectWellRouted(boxes, [{ id: "redis-web", source: "redis", target: "web" }], [bar]);
  });

  it("gives lines out of one side a port each, and lines into one card lanes of their own", () => {
    const boxes = new Map([["pg", box(0, 0)], ["api", box(400, -200)], ["worker", box(400, 0)], ["backup", box(400, 200)], ["web", box(800, 0)]]);
    expectWellRouted(boxes, [
      { id: "pg-api", source: "pg", target: "api" },
      { id: "pg-worker", source: "pg", target: "worker" },
      { id: "pg-backup", source: "pg", target: "backup" },
      { id: "pg-web", source: "pg", target: "web" },
      { id: "api-web", source: "api", target: "web" },
      { id: "backup-web", source: "backup", target: "web" },
    ]);
  });

  it("squeezes through a gap narrower than its clearance rather than under a card", () => {
    // A canvas as dragged: web sits 23px under postgres, api 43px left of it.
    const boxes = new Map([["api", box(-139, 57, 288, 144)], ["postgres", box(192, 24, 288, 184)], ["redis", box(0, -150, 288, 144)],
      ["web", box(96, 231, 288, 184)], ["worker", box(-247, 247, 288, 184)]]);
    expectWellRouted(boxes, [
      { id: "postgres-api", source: "postgres", target: "api" },
      { id: "api-web", source: "api", target: "web" },
      { id: "redis-web", source: "redis", target: "web" },
      { id: "redis-worker", source: "redis", target: "worker" },
    ]);
  });

  it("routes every line of a scattered canvas clear of every card", () => {
    for (let seed = 1; seed <= 40; seed++) {
      const next = random(seed);
      const boxes = new Map<string, Box>();
      while (boxes.size < 7) {
        const candidate = box(Math.round(next() * 1600), Math.round(next() * 1000), 200 + Math.round(next() * 160), 72 + Math.round(next() * 140));
        if ([...boxes.values()].some((other) => candidate.x < other.x + other.width + 60 && other.x < candidate.x + candidate.width + 60
          && candidate.y < other.y + other.height + 60 && other.y < candidate.y + candidate.height + 60)) continue;
        boxes.set(`n${boxes.size}`, candidate);
      }
      const ids = [...boxes.keys()];
      const edges = Array.from({ length: 8 }, (_, index) => {
        const source = must(ids[Math.floor(next() * ids.length)]);
        const others = ids.filter((id) => id !== source);
        return { id: `e${index}`, source, target: must(others[Math.floor(next() * others.length)]) };
      });
      expectWellRouted(boxes, edges);
    }
  });

  it("routes nothing for a line whose card isn't drawn, or one into itself", () => {
    const boxes = new Map([["api", box(0, 0)]]);
    expect(routeEdges({ boxes, edges: [{ id: "a", source: "gone", target: "api" }, { id: "b", source: "api", target: "api" }] }).size).toBe(0);
  });
});
