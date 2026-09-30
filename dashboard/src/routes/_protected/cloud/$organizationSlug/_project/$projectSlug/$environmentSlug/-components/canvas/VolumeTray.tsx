import { createContext, use, useState, type ReactNode } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { HardDriveIcon, LinkIcon } from "lucide-react";
import { listNames } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { useNodeLighting } from "../deployment-page";
import { useNodePick } from "../new-branch/branch-picking";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { fillText, fillTone, stagedSurface } from "./node-status";
import { FILL_CLASSES, STAGED_CLASSES } from "./node-status-view";
import { VolumeFillContext } from "./RuntimeLensProvider";
import type { MountedVolume } from "./types";

/** The Volume whose trays are lit: hovering a shared Volume's tray lights it under every Service that mounts it. */
const LitVolume = createContext<[string | null, (id: string | null) => void]>([null, () => {}]);

export function LitVolumeProvider({ children }: { children: ReactNode }) {
  return <LitVolume value={useState<string | null>(null)}>{children}</LitVolume>;
}

/**
 * A Volume as a tray tucked under a Service that mounts it: its name over a fill showing how full it is, and only what
 * must be said, green or blue when the next Deploy creates or changes it, red, struck and "Removing" when it deletes it,
 * "92% full" from 80%, marked when other Services mount it too. Opens its panel; while a Branch is picked, a click toggles it instead. Under an open Deployment Page it dims
 * unless the attempt changed it.
 */
export function VolumeTray({ tray: { volume, sharedWith, mountChanged }, selected }: { tray: MountedVolume; selected: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const [litVolumeId, setLitVolumeId] = use(LitVolume);
  const lighting = useNodeLighting(volume.id);
  const pick = useNodePick(volume.name);
  const fill = use(VolumeFillContext)(volume.id);
  const tone = fillTone(fill);
  // Staged by its own lifecycle first, else by the next Deploy adding or changing this mount of it.
  const surface = stagedSurface(lighting, volume.change ?? (mountChanged ? "update" : null));
  const alsoMounted = sharedWith.length > 0 ? `Also mounted by ${listNames(sharedWith)}` : undefined;
  const className = cn(
    "relative -mt-3 mx-1.5 flex h-13 items-end overflow-hidden gap-2 rounded-b-xl border border-t-0 bg-muted px-4 pb-2.5 text-xs text-muted-foreground",
    surface && STAGED_CLASSES[surface].surface,
    lighting === null && "opacity-40",
    alsoMounted && litVolumeId === volume.id && "border-foreground text-foreground",
    selected && "ring-2 ring-foreground",
    pick?.role === "own" && "ring-2 ring-foreground",
    pick?.role === "live" && "border-dashed",
    pick?.role === "left_out" && "opacity-40",
  );
  const content = <>
    {fill === null ? null : <span aria-hidden className={cn("absolute inset-y-0 left-0", FILL_CLASSES[tone ?? "ok"].bar)} style={{ width: `${fill * 100}%` }} />}
    <HardDriveIcon className="relative size-3.5 shrink-0" />
    <span className={cn("relative min-w-0 flex-1 truncate", surface && STAGED_CLASSES[surface].name)}>{volume.name}</span>
    {pick ? <span className="relative truncate">{pick.label}</span>
      : surface === "destructive" ? <span className="relative">Removing</span>
      : <>
        {tone && fill !== null ? <span className={cn("relative shrink-0", FILL_CLASSES[tone].text)}>{fillText(fill)}</span> : null}
        {alsoMounted ? <LinkIcon className="relative size-3.5 shrink-0" aria-label={alsoMounted} /> : null}
      </>}
  </>;
  const hover = { onMouseEnter: () => setLitVolumeId(volume.id), onMouseLeave: () => setLitVolumeId(null) };

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
      title={alsoMounted}>
      {content}
    </Link>
  );
}

/** A Service's Volume trays, under its card. */
export function ServiceTrays({ trays, selectedNodeId }: { trays: MountedVolume[]; selectedNodeId: string | null }) {
  return trays.map((tray) => <VolumeTray key={tray.volume.id} tray={tray} selected={tray.volume.id === selectedNodeId} />);
}
