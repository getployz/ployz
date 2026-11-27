import type { EdgeProps } from "@xyflow/react";
import { edgeRoute, RoutedEdge } from "#/modules/canvas/routed-edges";
import type { ServiceEdge as ServiceEdgeType } from "./types";

/** A link from a Service into one that reads it; while hovered or picked it darkens and says what is read. */
export function ServiceEdge(props: EdgeProps<ServiceEdgeType>) {
  const { id, data, style, markerEnd } = props;
  const { path, labelAt } = edgeRoute(props);
  const shown = data?.shown === true;
  return (
    <RoutedEdge
      id={id}
      path={path}
      labelAt={labelAt}
      markerEnd={markerEnd}
      style={shown ? { ...style, stroke: "var(--foreground)", strokeWidth: 2 } : style}
      label={shown && data ? (
        <div role="tooltip" className="max-w-xs rounded-md bg-foreground px-3 py-1.5 text-xs text-background">
          {data.keys.length === 1 ? (
            <p>{data.reader} references <span className="font-mono">{data.read}.{data.keys[0]}</span></p>
          ) : (
            <>
              <p>{data.reader} references</p>
              <ul>
                {data.keys.map((key) => <li key={key} className="font-mono">{data.read}.{key}</li>)}
              </ul>
            </>
          )}
        </div>
      ) : undefined}
    />
  );
}
