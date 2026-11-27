const BUFFER = 20;
const STEP = 50;

type Position = { x: number; y: number };
type Size = { width: number; height: number };
type NodeRect = Position & Size;

function overlaps(pos: Position, newSize: Size, node: NodeRect): boolean {
  return !(
    pos.x + newSize.width + BUFFER < node.x ||
    pos.x > node.x + node.width + BUFFER ||
    pos.y + newSize.height + BUFFER < node.y ||
    pos.y > node.y + node.height + BUFFER
  );
}

function hasOverlap(pos: Position, newSize: Size, nodes: NodeRect[]): boolean {
  return nodes.some((node) => overlaps(pos, newSize, node));
}

export function findPlacement(
  click: Position,
  newNodeSize: Size,
  existingNodes: NodeRect[],
): Position {
  if (!hasOverlap(click, newNodeSize, existingNodes)) {
    return click;
  }

  for (let radius = STEP; radius < 2000; radius += STEP) {
    for (let angle = 0; angle < Math.PI * 2; angle += Math.PI / 8) {
      const candidate = {
        x: click.x + Math.cos(angle) * radius,
        y: click.y + Math.sin(angle) * radius,
      };
      if (!hasOverlap(candidate, newNodeSize, existingNodes)) {
        return candidate;
      }
    }
  }

  return click;
}

const NEW_NODE_GAP = 48;
const NEW_NODE_COLUMNS = 4;
const GRID = 24;
const onGrid = (value: number) => Math.ceil(value / GRID) * GRID;

/**
 * Where nodes without a place go, in the order given: rows of up to four, below every placed node and lined up with the
 * leftmost one, or from the origin on an empty canvas. It never moves a placed node, and the same input gives the same
 * places, so two tabs placing the same nodes agree.
 */
export function placeNewNodes(placed: readonly NodeRect[], sizes: readonly Size[]): Position[] {
  const left = placed.length === 0 ? 0 : onGrid(Math.min(...placed.map((node) => node.x)));
  let top = placed.length === 0 ? 0 : onGrid(Math.max(...placed.map((node) => node.y + node.height)) + NEW_NODE_GAP);
  const places: Position[] = [];
  for (let start = 0; start < sizes.length; start += NEW_NODE_COLUMNS) {
    const row = sizes.slice(start, start + NEW_NODE_COLUMNS);
    let x = left;
    for (const size of row) {
      places.push({ x, y: top });
      x = onGrid(x + size.width + NEW_NODE_GAP);
    }
    top = onGrid(top + Math.max(...row.map((size) => size.height)) + NEW_NODE_GAP);
  }
  return places;
}
