import { createContext, use, useState, type ReactNode } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { HardDriveIcon, LinkIcon } from "lucide-react";
import { cn } from "#/lib/utils";
import { useNodeLighting } from "../deployment-page";
import { useNodePick } from "../new-branch/branch-picking";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import type { VolumeTray as Tray } from "./types";

/** The Volume whose trays are lit: hovering a shared Volume's tray lights it under every Service that mounts it. */
const LitVolume = createContext<[string | null, (id: string | null) => void]>([null, () => {}]);

export function LitVolumeProvider({ children }: { children: ReactNode }) {
  return <LitVolume value={useState<string | null>(null)}>{children}</LitVolume>;
}

/**
 * A Volume as a tray tucked under a Service that mounts it: its name, and only what must be said, pink when the next
 * Deploy changes it, struck and "Removing" when it deletes it, marked when other Services mount it too. Opens its panel;
 * while a Branch is picked, a click toggles it instead.
 */
export function VolumeTray({ tray: { volume, sharedWith }, selected }: { tray: Tray; selected: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const [lit, light] = use(LitVolume);
  const lighting = useNodeLighting(volume.id);
  const pick = useNodePick(volume.name);
  const removing = lighting === undefined && volume.change === "delete";
  const className = cn(
    "relative -mt-3 mx-1.5 flex h-13 items-end gap-2 rounded-b-xl border border-t-0 bg-muted px-4 pb-2.5 text-xs text-muted-foreground",
    lighting === undefined && volume.change !== null && (removing ? "border-destructive-border bg-destructive-soft text-destructive" : "border-changed-border bg-changed-soft text-changed-deep"),
    lighting === null && "opacity-40",
    sharedWith.length > 0 && lit === volume.id && "border-foreground text-foreground",
    selected && "ring-2 ring-foreground",
    pick?.role === "own" && "ring-2 ring-foreground",
    pick?.role === "live" && "border-dashed",
    pick?.role === "left_out" && "opacity-40",
  );
  const content = <>
    <HardDriveIcon className="size-3.5 shrink-0" />
    <span className={cn("min-w-0 flex-1 truncate", removing && "line-through")}>{volume.name}</span>
    {pick ? <span className="truncate">{pick.label}</span>
      : lighting ? <span className="truncate">{lighting.label}</span>
      : removing ? <span>Removing</span>
      : sharedWith.length > 0 ? <LinkIcon className="size-3.5 shrink-0" aria-label={`Also mounted by ${sharedWith.join(", ")}`} /> : null}
  </>;
  const hover = { onMouseEnter: () => light(volume.id), onMouseLeave: () => light(null) };

  if (pick) {
    return (
      <button type="button" aria-pressed={pick.role === "own"} aria-disabled={pick.fixed || undefined} data-canvas-node={volume.id} {...hover}
        className={cn(className, "w-[calc(100%-0.75rem)] text-left", pick.fixed ? "cursor-default" : "cursor-pointer")}
        onClick={() => { if (!pick.fixed) pick.toggle(); }}>
        {content}
      </button>
    );
  }
  return (
    <Link to={ENVIRONMENT_RESOURCE_ROUTE_TO} params={{ ...params, resourceId: volume.id }}
      search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
      data-canvas-node={volume.id} aria-current={selected ? "page" : undefined} draggable={false} className={className} {...hover}
      title={sharedWith.length > 0 ? `Also mounted by ${sharedWith.join(", ")}` : undefined}>
      {content}
    </Link>
  );
}
