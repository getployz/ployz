import { describe, expect, it } from "vitest";
import { placeNewNodes } from "./node-placement";

type Rect = { x: number; y: number; width: number; height: number };

const overlaps = (a: Rect, b: Rect) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;

/** A seeded canvas: what a person dragged where, and the sizes of the nodes nobody placed yet. */
function canvas(seed: number) {
  let state = seed;
  const next = (max: number) => {
    state = (state * 1103515245 + 12345) % 2147483648;
    return state % max;
  };
  const size = () => ({ width: 288, height: 144 + 40 * next(4) + (next(3) === 0 ? 80 : 0) });
  const placed = Array.from({ length: next(8) }, () => ({ x: next(2400) - 1200, y: next(1600) - 800, ...size() }));
  return { placed, sizes: Array.from({ length: 1 + next(12) }, size) };
}

describe("placing nodes nobody placed", () => {
  it("lays an empty canvas out in rows of four from the origin", () => {
    const places = placeNewNodes([], Array.from({ length: 6 }, () => ({ width: 288, height: 144 })));
    expect(places).toEqual([
      { x: 0, y: 0 }, { x: 336, y: 0 }, { x: 672, y: 0 }, { x: 1008, y: 0 },
      { x: 0, y: 192 }, { x: 336, y: 192 },
    ]);
  });

  it.each(Array.from({ length: 40 }, (_, seed) => seed + 1))("overlaps no placed node and no other new one (canvas %i)", (seed) => {
    const { placed, sizes } = canvas(seed);
    const places = placeNewNodes(placed, sizes);
    expect(places).toHaveLength(sizes.length);
    const rects = places.flatMap((place, index) => {
      const size = sizes[index];
      return size ? [{ ...place, ...size }] : [];
    });
    for (const [index, rect] of rects.entries()) {
      for (const node of placed) expect(overlaps(rect, node)).toBe(false);
      for (const other of rects.slice(index + 1)) expect(overlaps(rect, other)).toBe(false);
    }
  });
});
