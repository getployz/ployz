import { Link } from "@tanstack/react-router";
import { ChevronRightIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { cn } from "#/lib/utils";
import type { ServerStatus } from "#/modules/machines/server-status";
import type { Server } from "#/modules/machines/use-servers";
import type { ServerUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { runsHere, serviceNames } from "./runs-here";
import { ServerMeters } from "./server-meters";

/** What a Server row says about itself: a dot always, a visible word only when someone should look. */
export type ServerRowState = "online" | "building" | "offline" | "unknown" | "upgrading" | "upgrade-failed";

const STATES = {
  online: { word: null, dot: "bg-success", tone: "" },
  building: { word: "Building", dot: "bg-success", tone: "text-muted-foreground" },
  offline: { word: "Offline", dot: "bg-destructive", tone: "text-destructive" },
  unknown: { word: "Unknown", dot: "bg-muted-foreground", tone: "text-muted-foreground" },
  upgrading: { word: "Upgrading", dot: "animate-pulse bg-info", tone: "text-info" },
  "upgrade-failed": { word: "Upgrade failed", dot: "bg-warning", tone: "text-warning" },
} satisfies Record<ServerRowState, { word: string | null; dot: string; tone: string }>;

/**
 * A running upgrade explains whatever the Server reports meanwhile, so it wins. Offline and Unknown outrank a failed
 * upgrade. Behind and when-back show only in the top bar.
 */
export function serverRowState(status: ServerStatus, line: ServerUpgradeLine): ServerRowState {
  if (line?.kind === "upgrading") return "upgrading";
  if (status === "offline" || status === "unknown") return status;
  if (line?.kind === "failed") return "upgrade-failed";
  return status;
}

const MAX_CHIPS = 3;

/** The list's column template, shared by its rows and its skeleton so both line up. */
export const SERVER_ROW_GRID = cn(
  "grid grid-cols-[auto_minmax(0,1fr)_auto_auto] items-center gap-x-4 gap-y-1 px-3 py-2.5 text-sm",
  "@xl:grid-cols-[auto_minmax(0,1fr)_minmax(0,1.4fr)_7rem_auto]",
  "@4xl:grid-cols-[auto_minmax(0,1fr)_minmax(0,1.4fr)_19rem_7rem_auto]",
);

/** The bordered list that holds Server rows; its width, not the viewport's, decides each row's layout. */
export const SERVER_LIST = "@container flex flex-col divide-y overflow-hidden rounded-lg border";

/**
 * One Server as a row that opens its page: dot, name, what runs there, CPU and memory when the row is wide, and a
 * state word. A narrow row puts what runs under the name and drops the meters.
 */
export function ServerRow({ organizationSlug, server, upgrade, stale }: {
  organizationSlug: string;
  server: Server;
  upgrade: ServerUpgradeLine;
  stale: boolean;
}) {
  const { word, dot, tone } = STATES[serverRowState(server.status, upgrade)];
  const names = serviceNames(server);
  return (
    <Link
      to="/cloud/$organizationSlug/~/servers/$serverId"
      params={{ organizationSlug, serverId: server.machine.id }}
      className={cn(SERVER_ROW_GRID, "outline-none transition-colors hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:ring-inset")}
    >
      <span aria-hidden="true" className={cn("size-2 rounded-full", stale ? "bg-muted-foreground" : dot)} />
      <span className="truncate font-medium">{server.name}</span>
      <span className="col-2 row-2 flex min-w-0 items-center gap-1 overflow-hidden @xl:col-auto @xl:row-auto">
        {names.length === 0 ? <span className="truncate text-muted-foreground">{runsHere(server)}</span> : (
          <>
            {names.slice(0, MAX_CHIPS).map((name) => <Badge key={name} variant="secondary">{name}</Badge>)}
            {names.length > MAX_CHIPS ? <Badge variant="outline">+{names.length - MAX_CHIPS}</Badge> : null}
          </>
        )}
        {server.machine.storage === "stateless" ? <Badge variant="outline">Docker only</Badge> : null}
      </span>
      <span className="hidden @4xl:flex">
        <ServerMeters organizationSlug={organizationSlug} server={server} />
      </span>
      <span className={cn("col-3 row-[1/span_2] text-right whitespace-nowrap @xl:col-auto @xl:row-auto", stale ? "text-muted-foreground" : tone)}>
        {word ?? <span className="sr-only">Online</span>}
      </span>
      <ChevronRightIcon aria-hidden="true" className="col-4 row-[1/span_2] size-4 text-muted-foreground @xl:col-auto @xl:row-auto" />
    </Link>
  );
}
