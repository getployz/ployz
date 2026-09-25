import { useEffect, useState } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getDeploymentLogsCollection, useDeploymentLogsReadState } from "#/modules/deployments/deployment-log.collection";
import { progressRowLabel, type DeploymentProgress } from "#/modules/deployments/deployment-progress";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription } from "#/components/ui/item";
import { Spinner } from "#/components/ui/spinner";
import { CheckIcon, TriangleAlertIcon } from "lucide-react";
import { useBuildLog, type BuildOutputRow, type BuildStepRow } from "#/modules/deployments/deployment-build-log.queries";
import { BUILDING_KEY, CLEANUP_KEY } from "#/modules/deployments/preparation-progress";
import { builtOn, builtOnLine, imageBuildSteps, stripAnsi } from "#/modules/deployments/deployment-view";
import { ContainerLogs } from "./container-logs";
import type { ContainerLogRow } from "#/modules/runtime/container-log.collection";
import { BuildLogViewer } from "./log-scroll";
import { cn } from "#/lib/utils";

/** BuildKit names steps `[stage n/m] instruction`; Ployz-owned steps are plain. */
export function splitStepName(name: string): { stage: string | null; title: string } {
  const match = /^\[(?:([A-Za-z_][\w.-]*))?\s?(?:\d+\/\d+)?\]\s+(.*)$/s.exec(name);
  return match ? { stage: match[1] ?? null, title: match[2] ?? name } : { stage: null, title: name };
}

export function formatDuration(ms: number): string {
  if (ms < 1_000) return `${Math.max(0, Math.round(ms))}ms`;
  if (ms < 60_000) return `${Math.floor(ms / 1_000)}s`;
  return `${Math.floor(ms / 60_000)}m ${Math.floor((ms % 60_000) / 1_000)}s`;
}

function useNow(active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, [active]);
  return now;
}

export const clock = (date: Date) => date.toLocaleTimeString(undefined, { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });

const lastLine = (rows: readonly BuildOutputRow[]) => {
  const lines = stripAnsi(rows.map((row) => row.text).join("")).split("\n").filter((line) => line.trim());
  return lines.at(-1) ?? null;
};

export function BuildLogs({ steps, output, finished, now = Date.now() }: {
  steps: readonly BuildStepRow[]; output: readonly BuildOutputRow[]; finished: boolean; now?: number;
}) {
  // Rows the user toggled; failed rows open by default until toggled.
  const [toggled, setToggled] = useState<ReadonlyMap<number, boolean>>(new Map());
  const started = steps.filter((step) => step.startedAt !== null);
  if (!started.length) {
    return <p className="text-muted-foreground">{finished ? "No retained build output for this image." : "Waiting for the build to start"}</p>;
  }
  const outputByStep = new Map<number, BuildOutputRow[]>();
  for (const row of output) {
    const lines = outputByStep.get(row.stepId);
    if (lines) lines.push(row); else outputByStep.set(row.stepId, [row]);
  }
  // One attempt may run BuildKit several times; the run's heading matters only then, or when it failed.
  const runs = new Set(started.map((step) => step.build).filter((build) => build > 0)).size;
  const failedRuns = new Set(steps.filter((step) => step.error !== null).map((step) => step.build));
  const shown = started.filter((step) => step.error !== null || (step.key !== CLEANUP_KEY && (step.key !== BUILDING_KEY || runs > 1 || failedRuns.has(step.build))));
  return <ol>
    {shown.map((step) => step.key === BUILDING_KEY && step.error === null
      ? <li key={step.id} className="mt-2 flex items-center gap-3 px-1 font-medium"><span className="w-16 shrink-0" /><span className="w-4 shrink-0" />Building {step.name}</li>
      : <StepRow key={step.id} step={step} lines={outputByStep.get(step.id) ?? []} now={now} open={toggled.get(step.id)}
          onToggle={(open) => setToggled((previous) => previous.get(step.id) === open ? previous : new Map(previous).set(step.id, open))} />)}
  </ol>;
}

function StepRow({ step, lines, now, open: toggledOpen, onToggle }: {
  step: BuildStepRow; lines: readonly BuildOutputRow[]; now: number; open: boolean | undefined; onToggle: (open: boolean) => void;
}) {
  const { stage, title } = splitStepName(step.name);
  const failed = step.error !== null;
  const running = !failed && step.completedAt === null;
  const open = toggledOpen ?? failed;
  const elapsed = step.startedAt ? (step.completedAt?.getTime() ?? now) - step.startedAt.getTime() : 0;
  const tail = running && !open ? lastLine(lines) : null;
  const summary = <>
    <span className="w-16 shrink-0 text-muted-foreground">{clock(step.startedAt ?? step.createdAt)}</span>
    <span className="flex w-4 shrink-0 justify-center">
      {failed ? <TriangleAlertIcon className="size-4 text-destructive" aria-label="Failed" /> : running ? <Spinner /> : <CheckIcon className="size-4 text-muted-foreground" aria-label="Completed" />}
    </span>
    {stage ? <span className="w-16 shrink-0 truncate text-muted-foreground">{stage}</span> : null}
    <span className={cn("min-w-0 flex-1 truncate", failed && "text-destructive")}>{title}{step.cached ? <span className="ml-2 text-muted-foreground">cached</span> : null}</span>
    <span className="shrink-0 text-muted-foreground">{formatDuration(elapsed)}</span>
  </>;
  const row = "flex items-center gap-3 rounded px-1";
  if (!lines.length && !failed) return <li><div className={row}>{summary}</div></li>;
  return <li className={cn(failed && "border-l-2 border-destructive")}>
    <details open={open} onToggle={(event) => onToggle(event.currentTarget.open)}>
      <summary className={cn(row, "cursor-pointer list-none hover:bg-muted/40 [&::-webkit-details-marker]:hidden")}>{summary}</summary>
      {lines.length ? <pre className="whitespace-pre-wrap break-words pl-24">{lines.map((line) => <span key={line.id} className={line.stderr ? "text-foreground" : "text-muted-foreground"}>{stripAnsi(line.text)}</span>)}</pre> : null}
      {step.error ? <p className="whitespace-pre-wrap break-words pl-24 text-destructive">{step.error}</p> : null}
    </details>
    {tail ? <pre className="truncate pl-24 text-muted-foreground">{tail}</pre> : null}
  </li>;
}

function lifecycleLogs(events: readonly { id: number; createdAt: Date; progress: DeploymentProgress }[], serviceId: string): ContainerLogRow[] {
  const previous = new Map<number, string>();
  const logs: ContainerLogRow[] = [];
  for (const event of events) {
    for (const row of event.progress.rows) {
      if (row.serviceId !== serviceId && row.serviceId !== null) continue;
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

/** One service's Build logs in an attempt: only its own Image Build, which the engine names after the service's private DNS name. */
export function ServiceBuildLogs({ organizationSlug, deploymentId, image }: { organizationSlug: string; deploymentId: string; image: string }) {
  const build = useBuildLog(organizationSlug, deploymentId);
  const now = useNow(build.data?.finished === false);
  const steps = imageBuildSteps(build.data?.steps ?? [], image);
  const ids = new Set(steps.map((step) => step.id));
  const server = builtOn(build.data, image);
  return <>
    {build.isError ? <p role="alert">Could not load build logs. <Button variant="ghost" size="sm" disabled={build.isFetching} onClick={() => void build.refetch()}>Retry</Button></p> : null}
    {server ? <Item size="xs">
      <ItemContent><ItemDescription>{builtOnLine(server)}</ItemDescription></ItemContent>
      {server.server !== null && server.runUrl ? <ItemActions>
        <Button variant="link" size="xs" nativeButton={false} render={<a href={server.runUrl} target="_blank" rel="noreferrer" />}>View run ↗</Button>
      </ItemActions> : null}
    </Item> : null}
    <BuildLogViewer key={`${deploymentId}:${image}`}>
      {build.isPending ? <p>Loading logs…</p> : <BuildLogs steps={steps} output={(build.data?.output ?? []).filter((row) => ids.has(row.stepId))} finished={build.data?.finished ?? true} now={now} />}
    </BuildLogViewer>
  </>;
}

/** One service's Deploy logs in an attempt: its rollout steps interleaved with the attempt's container output. */
export function ServiceDeployLogs({ organizationSlug, deploymentId, serviceId, finished }: { organizationSlug: string; deploymentId: string; serviceId: string; finished: boolean }) {
  const collection = getDeploymentLogsCollection(organizationSlug, deploymentId, useCollectionScope());
  const { data: events = [] } = useLiveQuery({ queryKey: ['deployment-events', collection.id], query: (q) => q.from({ event: collection }).orderBy(({ event }) => event.id, "asc") });
  const request = useDeploymentLogsReadState(collection);
  return <>
    {request.isError ? <p role="alert">Could not load deployment logs. <Button variant="ghost" size="sm" disabled={request.isFetching} onClick={() => void collection.utils.refetch()}>Retry</Button></p> : null}
    <ContainerLogs selection={{ organizationSlug, deploymentId, serviceId }} lifecycle={lifecycleLogs(events, serviceId)} finished={finished} />
  </>;
}
