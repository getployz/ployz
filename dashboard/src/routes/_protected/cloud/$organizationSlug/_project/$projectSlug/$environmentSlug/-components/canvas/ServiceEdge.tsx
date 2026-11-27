import { BaseEdge, EdgeLabelRenderer, getSmoothStepPath, type EdgeProps } from "@xyflow/react";
import type { ServiceEdge as ServiceEdgeType } from "./types";

/** A link from a Service into one that reads it; while hovered or picked it darkens and says what is read. */
export function ServiceEdge({ id, sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition, markerEnd, style, data }:
  EdgeProps<ServiceEdgeType>) {
  const [path, labelX, labelY] = getSmoothStepPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition });
  const shown = data?.shown === true;
  return (
    <>
      <BaseEdge id={id} path={path} markerEnd={markerEnd} style={shown ? { ...style, stroke: "var(--foreground)", strokeWidth: 2 } : style} />
      {shown && data && (
        <EdgeLabelRenderer>
          <div
            role="tooltip"
            className="pointer-events-none absolute z-10 max-w-xs rounded-md bg-foreground px-3 py-1.5 text-xs text-background"
            style={{ transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)` }}
          >
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
        </EdgeLabelRenderer>
      )}
    </>
  );
}
