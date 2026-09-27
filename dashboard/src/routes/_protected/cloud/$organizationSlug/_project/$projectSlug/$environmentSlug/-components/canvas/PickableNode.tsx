import type { ReactNode } from "react";
import { Handle, Position } from "@xyflow/react";
import { cn } from "#/lib/utils";
import type { NodePick } from "../new-branch/branch-picking";

/** A canvas card while a Branch is being picked: clicking it (or Enter/Space) toggles its Own Copy. */
export function PickableNode({ pick, name, nodeId, children }: { pick: NodePick; name: string; nodeId: string; children: ReactNode }) {
  return (
    <button
      type="button"
      aria-pressed={pick.role === "own"}
      aria-disabled={pick.fixed || undefined}
      aria-label={`${name}, ${pick.label}${pick.ownsData ? ", real data" : ""}`}
      data-canvas-node={nodeId}
      className={cn("block h-36 w-72 rounded-xl text-left outline-none focus-visible:ring-3 focus-visible:ring-ring",
        pick.fixed ? "cursor-default" : "cursor-pointer")}
      onClick={() => { if (!pick.fixed) pick.toggle(); }}
    >
      <Handle type="target" position={Position.Bottom} isConnectable={false} className="opacity-0" />
      <Handle type="source" position={Position.Top} isConnectable={false} className="opacity-0" />
      {children}
    </button>
  );
}

/** The card state and fade for what a node becomes in the Branch being picked. */
export function pickCard(pick: NodePick) {
  return {
    state: pick.role === "own" ? "own" as const : pick.role === "live" ? "live" as const : undefined,
    className: pick.role === "left_out" ? "opacity-40" : undefined,
  };
}

/** What a node becomes, in words, with an amber "real data" line for a Live Node that owns data. */
export function LiveLabel({ label, ownsData }: { label: string; ownsData: boolean }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <span className="truncate text-muted-foreground">{label}</span>
      {ownsData ? <span className="truncate text-warning">real data</span> : null}
    </div>
  );
}
