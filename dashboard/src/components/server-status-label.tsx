import type { ReactNode } from "react";
import { cn } from "#/lib/utils";
import type { ServerStatus } from "#/modules/machines/server-status";

const LABELS = {
  online: { word: "Online", dot: "bg-success" },
  building: { word: "Building", dot: "bg-info" },
  not_responding: { word: "Not responding", dot: "bg-warning" },
  offline: { word: "Offline", dot: "bg-destructive" },
  unknown: { word: "Unknown", dot: "bg-muted-foreground" },
} satisfies Record<ServerStatus, { word: string; dot: string }>;

/** A Server's status: a dot and one word. A stale observation keeps the word and greys the dot. */
export function ServerStatusLabel({ status, stale = false, children }: { status: ServerStatus; stale?: boolean; children?: ReactNode }) {
  const { word, dot } = LABELS[status];
  return (
    <span className={cn("inline-flex items-center gap-2 text-muted-foreground", status === "offline" && !stale && "text-destructive")}>
      <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", stale ? "bg-muted-foreground" : dot)} />
      {children ?? word}
    </span>
  );
}
