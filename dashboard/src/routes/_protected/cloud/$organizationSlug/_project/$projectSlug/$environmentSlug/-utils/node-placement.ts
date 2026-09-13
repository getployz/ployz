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
