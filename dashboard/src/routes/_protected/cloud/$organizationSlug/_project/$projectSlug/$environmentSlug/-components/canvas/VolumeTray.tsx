import { createContext, use, useState, type ReactNode } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { FolderIcon, HardDriveIcon, LinkIcon } from "lucide-react";
import { listNames } from "#/lib/plural";
import { cn } from "#/lib/utils";
import { useNodeLighting } from "../deployment-page";
import { useNodePick } from "../new-branch/branch-picking";
import { ENVIRONMENT_RESOURCE_ROUTE_TO, ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { stagedSurface } from "./node-status";
import { STAGED_CLASSES } from "./node-status-view";
import type { MountedVolume } from "./types";
import type { MountedConfig } from "#/modules/config-store/store-configs";
import { SHARED_VOLUME_WHY } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

/** The Volume whose trays are lit: hovering a shared Volume's tray lights it under every Service that mounts it. */
const LitVolume = createContext<[string | null, (id: string | null) => void]>([null, () => {}]);

export function LitVolumeProvider({ children }: { children: ReactNode }) {
  return <LitVolume value={useState<string | null>(null)}>{children}</LitVolume>;
}

/**
 * A Volume as a tray tucked under a Service that mounts it: its name, and only what must be said, green or blue when
 * the next Deploy creates or changes it, red, struck and "Removing" when it deletes it, marked when other Services mount
 * it too. Opens its panel; while a Branch is picked, a click toggles it instead. Under an open Deployment Page it dims
 * unless the attempt changed it.
 */
export function VolumeTray({ tray: { volume, sharedWith, mountChanged, writers }, selected }: { tray: MountedVolume; selected: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const [litVolumeId, setLitVolumeId] = use(LitVolume);
  const lighting = useNodeLighting(volume.id);
  const pick = useNodePick(volume.name);
  // Staged by its own lifecycle first, else by the next Deploy adding or changing this mount of it.
  const surface = stagedSurface(lighting, volume.change ?? (mountChanged ? "update" : null));
  const alsoMounted = sharedWith.length > 0 ? `Also mounted by ${listNames(sharedWith)}` : undefined;
  // Several writers on one directory must be said: it's how data gets corrupted.
  const shared = writers > 1;
  const className = cn(
    "relative -mt-3 mx-1.5 flex h-13 items-end gap-2 rounded-b-xl border border-t-0 bg-muted px-4 pb-2.5 text-xs text-muted-foreground",
    surface && STAGED_CLASSES[surface].surface,
    lighting === null && "opacity-40",
    alsoMounted && litVolumeId === volume.id && "border-foreground text-foreground",
    selected && "ring-2 ring-foreground",
    pick?.role === "own" && "ring-2 ring-foreground",
    pick?.role === "live" && "border-dashed",
    pick?.role === "left_out" && "opacity-40",
  );
  const content = <>
    <HardDriveIcon className="size-3.5 shrink-0" />
    <span className={cn("min-w-0 flex-1 truncate", surface && STAGED_CLASSES[surface].name)}>{volume.name}</span>
    {pick ? <span className="truncate">{pick.label}</span>
      : surface === "destructive" ? <span>Removing</span>
      : shared ? <span className="shrink-0 text-warning">shared · {writers} writers</span>
      : alsoMounted ? <LinkIcon className="size-3.5 shrink-0" aria-label={alsoMounted} /> : null}
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
      title={shared ? SHARED_VOLUME_WHY : alsoMounted}>
      {content}
    </Link>
  );
}

/** A Config as a tray under a Service that mounts it: its name and the directory it lands in. Opens its panel. */
export function ConfigTray({ tray: { config, dir, mountChanged }, selected }: { tray: MountedConfig; selected: boolean }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const lighting = useNodeLighting(config.id);
  const surface = stagedSurface(lighting, config.change ?? (mountChanged ? "update" : null));
  return (
    <Link to={ENVIRONMENT_RESOURCE_ROUTE_TO} params={{ ...params, resourceId: config.id }}
      search={(prev) => ({ ...prev, tab: selected ? prev.tab : undefined })}
      data-canvas-node={config.id} aria-current={selected ? "page" : undefined} draggable={false}
      preload="intent"
      className={cn(
        "relative -mt-3 mx-1.5 flex h-13 items-end gap-2 rounded-b-xl border border-t-0 bg-muted px-4 pb-2.5 text-xs text-muted-foreground",
        surface && STAGED_CLASSES[surface].surface,
        lighting === null && "opacity-40",
        selected && "ring-2 ring-foreground",
      )}>
      <FolderIcon className="size-3.5 shrink-0" />
      <span className={cn("shrink-0", surface && STAGED_CLASSES[surface].name)}>{config.name}</span>
      <span className="min-w-0 flex-1 truncate text-right font-mono">{surface === "destructive" ? "Removing" : dir}</span>
    </Link>
  );
}

/** A Service's trays under its card: its Configs, then its Volumes. */
export function ServiceTrays({ trays, configTrays, selectedNodeId }:
  { trays: MountedVolume[]; configTrays: MountedConfig[]; selectedNodeId: string | null }) {
  return <>
    {configTrays.map((tray) => <ConfigTray key={tray.config.id} tray={tray} selected={tray.config.id === selectedNodeId} />)}
    {trays.map((tray) => <VolumeTray key={tray.volume.id} tray={tray} selected={tray.volume.id === selectedNodeId} />)}
  </>;
}
