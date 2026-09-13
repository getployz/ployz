import { useState } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { reconcileCollection } from "#/collections/query-collection";
import { getDeploymentLogsCollection } from "#/modules/deployments/deployment-log.collection";
import { progressRowLabel } from "#/modules/deployments/deployment-progress";
import { Button } from "#/components/ui/button";
import { cn } from "#/lib/utils";

export function DeploymentLogs({ organizationSlug, deploymentId, serviceId }: { organizationSlug: string; deploymentId: string; serviceId?: string }) {
  const collection = getDeploymentLogsCollection(organizationSlug, deploymentId, useCollectionScope());
  const { data: events = [], isLoading, isError } = useLiveQuery((q) => q.from({ event: collection }).orderBy(({ event }) => event.id, "asc"));
  const [tab, setTab] = useState<"Build logs" | "Deploy logs">("Deploy logs");
  const [refreshing, setRefreshing] = useState(false);
  const [refreshError, setRefreshError] = useState(false);
  const previous = new Map<number, string>();
  const logs: { id: string; at: Date; message: string }[] = [];
  for (const event of events) {
    for (const row of event.progress.rows) {
      if (serviceId && row.serviceId !== serviceId && row.serviceId !== null) continue;
      const label = `${row.status} · ${progressRowLabel(row)}${row.health ? ` · ${row.health}` : ""}`;
      if (previous.get(row.index) === label) continue;
      previous.set(row.index, label);
      logs.push({ id: `${event.id}:${row.index}`, at: event.createdAt, message: `${row.serviceName ?? "Environment"} · ${row.machineName ?? row.machineId} · ${row.displayName ?? row.target ?? `operation ${row.index + 1}`} · ${label}${row.elapsedMs !== null ? ` · ${Math.floor(row.elapsedMs / 1000)}s / ${Math.floor((row.deadlineMs ?? 0) / 1000)}s` : ""}` });
    }
    event.progress.compensation.forEach((message, i) => logs.push({ id: `${event.id}:recovery:${i}`, at: event.createdAt, message }));
  }
  async function refresh() {
    setRefreshing(true); setRefreshError(false);
    try { await reconcileCollection(collection); } catch { setRefreshError(true); } finally { setRefreshing(false); }
  }
  return <div className="rounded-lg bg-background p-4">
    <div className="mb-3 flex items-center gap-4">{(["Build logs", "Deploy logs"] as const).map((t) => <button key={t} type="button" className={cn("text-xs underline-offset-8", tab === t ? "underline" : "text-muted-foreground")} aria-pressed={tab === t} onClick={() => setTab(t)}>{t}</button>)}<Button className="ml-auto" variant="ghost" size="sm" disabled={refreshing || isLoading} onClick={() => void refresh()}>Refresh logs</Button></div>
    {isError || refreshError ? <p role="alert" className="text-xs text-destructive">Could not load logs. Try refreshing.</p> : null}
    <div className="max-h-80 overflow-auto break-words font-mono text-xs leading-6" tabIndex={0} aria-label={tab}>
      {tab === "Build logs" ? <p className="text-muted-foreground">This deployment uses prebuilt images. No build logs were produced.</p> : isLoading ? <p>Loading logs…</p> : logs.length ? logs.map((l) => <p key={l.id}><time className="mr-3 text-muted-foreground" dateTime={l.at.toISOString()}>{l.at.toLocaleTimeString()}</time>{l.message}</p>) : <p className="text-muted-foreground">No retained runtime events for this deployment.</p>}
    </div>
  </div>;
}
