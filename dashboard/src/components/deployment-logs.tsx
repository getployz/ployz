import { useState } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useQuery } from "@tanstack/react-query";
import { getDeploymentLogsCollection } from "#/modules/deployments/deployment-log.collection";
import { progressRowLabel, type DeploymentProgress } from "#/modules/deployments/deployment-progress";
import { Button } from "#/components/ui/button";
import { ContainerLogs } from "./container-logs";
import type { ContainerLogRow } from "#/modules/runtime/container-log.collection";
import { BuildLogViewer } from "./log-scroll";
import { cn } from "#/lib/utils";

export function BuildLogs({ events, serviceId, hasBuild }: {
  events: readonly { id: number; progress: DeploymentProgress }[]; serviceId?: string; hasBuild: boolean;
}) {
  const preparation = events.flatMap((event) => event.progress.preparation && (!serviceId || event.progress.preparation.serviceId === serviceId || event.progress.preparation.serviceId === null) ? [{ id: event.id, ...event.progress.preparation }] : []);
  const truncated = preparation.some((event) => event.outputTruncated);
  return <>
    {preparation.map((event) => event.message ? <p key={event.id}>{event.message}</p> : null)}
    <pre className="whitespace-pre-wrap break-words">{preparation.map((event) => event.output).join("")}</pre>
    {truncated ? <p role="status">Build output truncated. Only retained output is shown.</p> : null}
    {!preparation.length ? <p className="text-muted-foreground">{hasBuild ? "No retained build output for this deployment." : "This deployment uses prebuilt images. No build logs were produced."}</p> : null}
  </>;
}

function lifecycleLogs(events: readonly { id: number; createdAt: Date; progress: DeploymentProgress }[], serviceId?: string): ContainerLogRow[] {
  const previous = new Map<number, string>();
  const logs: ContainerLogRow[] = [];
  for (const event of events) {
    for (const row of event.progress.rows) {
      if (serviceId && row.serviceId !== serviceId && row.serviceId !== null) continue;
      if (row.status === "pending" || row.status === "unexecuted" || row.phase === "waiting_for_health") continue;
      const label = row.status === "failed" ? (row.error ?? "Deployment failed") : progressRowLabel(row);
      if (previous.get(row.index) === label) continue;
      previous.set(row.index, label);
      logs.push({ id: `lifecycle:${event.id}:${row.index}`, timestamp: String(BigInt(event.createdAt.getTime()) * 1_000_000n), channel: "lifecycle", machineId: row.machineId, machineName: row.machineName ?? row.machineId, containerId: row.target ?? "", serviceName: row.serviceName ?? "Environment", message: label });
    }
    event.progress.compensation.forEach((message, i) => logs.push({ id: `lifecycle:${event.id}:recovery:${i}`, timestamp: String(BigInt(event.createdAt.getTime()) * 1_000_000n), channel: "lifecycle", machineId: "", machineName: "", containerId: "", serviceName: "Deployment", message }));
  }
  return logs;
}

export function DeploymentLogs({ organizationSlug, deploymentId, serviceId, hasBuild }: { organizationSlug: string; deploymentId: string; serviceId?: string; hasBuild: boolean }) {
  const collection = getDeploymentLogsCollection(organizationSlug, deploymentId, useCollectionScope());
  const { data: events = [] } = useLiveQuery({ queryKey: ['deployment-events', collection.id], query: (q) => q.from({ event: collection }).orderBy(({ event }) => event.id, "asc") });
  const [tab, setTab] = useState<"Build logs" | "Deploy logs">(hasBuild ? "Build logs" : "Deploy logs");
  const request = useQuery({ ...collection.queryOptions, enabled: false });
  const logs = lifecycleLogs(events, serviceId);
  return <div className="rounded-lg bg-background p-4">
    <div className="mb-3 flex items-center gap-4">{(["Build logs", "Deploy logs"] as const).map((t) => <button key={t} type="button" className={cn("text-xs underline-offset-8", tab === t ? "underline" : "text-muted-foreground")} aria-pressed={tab === t} onClick={() => setTab(t)}>{t}</button>)}</div>
    {request.isError ? <p role="alert">Could not load deployment logs. <Button variant="ghost" size="sm" disabled={request.isFetching} onClick={() => void collection.utils.refetch()}>Retry</Button></p> : null}
    {tab === "Deploy logs" ? <ContainerLogs selection={{ organizationSlug, deploymentId, serviceId }} lifecycle={logs} /> : <BuildLogViewer key={`${deploymentId}:${serviceId ?? "all"}`}>
      {!events.length && request.isPending ? <p>Loading logs…</p> : <BuildLogs events={events} serviceId={serviceId} hasBuild={hasBuild} />}
    </BuildLogViewer>}
  </div>;
}
