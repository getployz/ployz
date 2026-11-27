import { createContext, useCallback, useEffect, useMemo, useState, type ReactNode, type RefCallback } from "react";
import {
  BaseEdge, EdgeLabelRenderer, getSmoothStepPath, useStore, useStoreApi,
  type Edge, type EdgeProps, type ReactFlowState,
} from "@xyflow/react";
import { routeEdges, type Box, type EdgeRoute, type Point } from "./edge-routes";

export type RoutedData = { route?: EdgeRoute };

/** A line drawn along `path`, with `label` centred on `labelAt` when it has one. */
export function RoutedEdge({ id, path, labelAt, label, style, markerEnd }: {
  id: string; path: string; labelAt: Point; label?: ReactNode; style?: EdgeProps["style"]; markerEnd?: string;
}) {
  return (
    <>
      <BaseEdge id={id} path={path} style={style} markerEnd={markerEnd} />
      {label ? (
        <EdgeLabelRenderer>
          <div className="pointer-events-none absolute z-10" style={{ transform: `translate(-50%, -50%) translate(${labelAt.x}px, ${labelAt.y}px)` }}>
            {label}
          </div>
        </EdgeLabelRenderer>
      ) : null}
    </>
  );
}

/** The route an edge was given; until its cards are measured, a smooth step between its handles. */
export function edgeRoute({ data, sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition }: EdgeProps<Edge<RoutedData>>) {
  if (data?.route) return data.route;
  const [path, x, y] = getSmoothStepPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition });
  return { path, labelAt: { x, y } };
}

/** A line with nothing to say: its route and its style. */
export function RoutedStepEdge(props: EdgeProps<Edge<RoutedData>>) {
  const { path, labelAt } = edgeRoute(props);
  return <RoutedEdge id={props.id} path={path} labelAt={labelAt} style={props.style} markerEnd={props.markerEnd} />;
}

type Measured = readonly [id: string, x: number, y: number, width: number, height: number];

const measuredBoxes = (state: ReactFlowState): Measured[] => [...state.nodeLookup.values()].flatMap((node) => node.hidden ? [] : [[
  node.id, node.internals.positionAbsolute.x, node.internals.positionAbsolute.y,
  node.measured.width ?? node.width ?? 0, node.measured.height ?? node.height ?? 0,
] as const]);

const sameBoxes = (a: Measured[], b: Measured[]) => a.length === b.length && a.every((box, index) => {
  const other = b[index];
  return other !== undefined && box.every((value, at) => value === other[at]);
});

/**
 * `edges`, each with a route around every card where the cards stand now and around `keepOut`, as `RoutedEdge` draws it.
 * An edge with no type of its own is a `routed` one. Routes follow a card as it moves or grows.
 */
export function useRoutedEdges<E extends Edge>(edges: readonly E[], keepOut: readonly Box[]): E[] {
  const measured = useStore(measuredBoxes, sameBoxes);
  // Which cards each edge links, as one string: an unchanged canvas rebuilds its edges every render.
  const ends = edges.map(({ id, source, target }) => `${id}\n${source}\n${target}`).join("\n\n");
  const routes = useMemo(() => routeEdges({
    boxes: new Map(measured.map(([id, x, y, width, height]) => [id, { x, y, width, height }])),
    edges: ends ? ends.split("\n\n").map((edge) => {
      const [id = "", source = "", target = ""] = edge.split("\n");
      return { id, source, target };
    }) : [],
    keepOut,
  }), [measured, ends, keepOut]);
  return edges.map((edge) => ({ ...edge, type: edge.type ?? "routed", data: { ...edge.data, route: routes.get(edge.id) } }));
}

/** Where a canvas's floating bars register, so lines keep out from under them. */
export const CanvasKeepOut = createContext<RefCallback<HTMLElement> | undefined>(undefined);

/** How long the view has to hold still before the bars are measured again where it now shows them. */
const SETTLE_MS = 120;

/**
 * The floating bars over a canvas in canvas coordinates, measured again whenever one resizes or the view settles after
 * moving; `keepOutRef` registers a bar, through `CanvasKeepOut` for one rendered further down.
 */
export function useKeepOut() {
  const [elements, setElements] = useState<ReadonlySet<HTMLElement>>(new Set());
  const keepOutRef = useCallback<RefCallback<HTMLElement>>((element) => {
    if (!element) return;
    setElements((current) => new Set(current).add(element));
    return () => setElements((current) => {
      const next = new Set(current);
      next.delete(element);
      return next;
    });
  }, []);
  const store = useStoreApi();
  const [keepOut, setKeepOut] = useState<Box[]>([]);
  useEffect(() => {
    const measure = () => {
      const { domNode, transform: [panX, panY, zoom] } = store.getState();
      if (!domNode) return;
      const canvas = domNode.getBoundingClientRect();
      const next = [...elements].map((element) => element.getBoundingClientRect())
        .filter((rect) => rect.width && rect.height && rect.right > canvas.left && rect.left < canvas.right
          && rect.bottom > canvas.top && rect.top < canvas.bottom)
        .map((rect) => ({
          x: Math.round((rect.left - canvas.left - panX) / zoom), y: Math.round((rect.top - canvas.top - panY) / zoom),
          width: Math.round(rect.width / zoom), height: Math.round(rect.height / zoom),
        }));
      setKeepOut((current) => JSON.stringify(current) === JSON.stringify(next) ? current : next);
    };
    measure();
    const observer = new ResizeObserver(measure);
    for (const element of elements) observer.observe(element);
    const { domNode } = store.getState();
    if (domNode) observer.observe(domNode);
    let settling: ReturnType<typeof setTimeout> | undefined;
    const unsubscribe = store.subscribe((state, previous) => {
      if (state.transform === previous.transform) return;
      clearTimeout(settling);
      settling = setTimeout(measure, SETTLE_MS);
    });
    return () => {
      observer.disconnect();
      unsubscribe();
      clearTimeout(settling);
    };
  }, [elements, store]);
  return { keepOut, keepOutRef };
}
