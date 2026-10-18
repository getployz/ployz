import type { ServerListItem } from "#/modules/machines/use-servers";
import type { ServerUpgradeLine } from "#/modules/server-upgrade/server-upgrade";

const PARTS = [
  { key: "online", label: "online", dot: "bg-success" },
  { key: "building", label: "building", dot: "bg-success" },
  { key: "offline", label: "offline", dot: "bg-destructive" },
  { key: "unknown", label: "unknown", dot: "bg-muted-foreground" },
  { key: "behind", label: "needs upgrade", dot: "bg-warning" },
] as const;

/** Servers per status, plus those behind their release line. Behind overlaps the status counts. */
export function fleetHealth(servers: readonly ServerListItem[], upgrades: ReadonlyMap<string, ServerUpgradeLine>) {
  const counts = { online: 0, building: 0, offline: 0, unknown: 0, behind: 0 };
  for (const server of servers) {
    counts[server.status] += 1;
    const line = upgrades.get(server.machine.id);
    if (line?.kind === "behind" || line?.kind === "when-back") counts.behind += 1;
  }
  return counts;
}

export function FleetHealth({ servers, upgrades }: { servers: readonly ServerListItem[]; upgrades: ReadonlyMap<string, ServerUpgradeLine> }) {
  const counts = fleetHealth(servers, upgrades);
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
      <h2 className="text-sm font-semibold">
        Fleet <span className="font-normal text-muted-foreground tabular-nums">{servers.length}</span>
      </h2>
      <ul aria-label="Fleet health" className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground tabular-nums">
        {PARTS.filter((part) => counts[part.key] > 0).map((part) => (
          <li key={part.key} className="flex items-center gap-1.5">
            <span aria-hidden="true" className={`size-1.5 rounded-full ${part.dot}`} />
            {counts[part.key]} {part.label}
          </li>
        ))}
      </ul>
    </div>
  );
}
