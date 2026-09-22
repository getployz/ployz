import { useEffect, useState } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { getDeploymentLogsCollection } from "#/modules/deployments/deployment-log.collection";
import { progressRowLabel, type DeploymentProgress } from "#/modules/deployments/deployment-progress";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { CheckIcon, TriangleAlertIcon } from "lucide-react";
import { listDeploymentBuildLogServerFn } from "#/modules/deployments/deployment.functions";
import { ContainerLogs } from "./container-logs";
import type { ContainerLogRow } from "#/modules/runtime/container-log.collection";
import { BuildLogViewer } from "./log-scroll";
import { cn } from "#/lib/utils";

type BuildLogPage = Awaited<ReturnType<typeof listDeploymentBuildLogServerFn>>;
export type BuildStepRow = BuildLogPage["steps"][number];
export type BuildOutputRow = BuildLogPage["output"][number];

type BuildLog = { steps: BuildStepRow[]; output: BuildOutputRow[]; finished: boolean };

/** Polls the step tree while the attempt runs, resuming output from the last row already held. */
function useBuildLog(organizationSlug: string, deploymentId: string, enabled: boolean) {
  const queryClient = useQueryClient();
  const queryKey = ["deployment-build-log", organizationSlug, deploymentId];
  return useQuery<BuildLog>({
    queryKey,
    enabled,
    refetchInterval: (query) => query.state.data?.finished ? false : 2_000,
    queryFn: async ({ signal }) => {
      const previous = queryClient.getQueryData<BuildLog>(queryKey);
      const steps: BuildStepRow[] = [];
      const output: BuildOutputRow[] = [...previous?.output ?? []];
      let finished = false;
      const last = output.at(-1);
      let afterSequence: string | null | undefined = last ? String(last.id) : undefined;
      while (afterSequence !== null) {
        const page = await listDeploymentBuildLogServerFn({ data: { organizationSlug, deploymentId, afterSequence, limit: 100 }, signal });
        steps.splice(0, steps.length, ...page.steps);
        output.push(...page.output);
        finished = page.finished;
        afterSequence = page.nextSequence;
      }
      return { steps, output, finished };
    },
  });
}

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

/** Terminal colour and cursor sequences carry nothing the log needs. */
const ESC = String.fromCharCode(27);
const BEL = String.fromCharCode(7);
const ansi = new RegExp(`${ESC}(?:\\[[0-?]*[ -/]*[@-~]|\\][^${BEL}]*(?:${BEL}|${ESC}\\\\)|[@-Z\\\\-_])`, "g");
export const stripAnsi = (text: string) => text.replaceAll(ansi, "");

const lastLine = (rows: readonly BuildOutputRow[]) => {
  const lines = stripAnsi(rows.map((row) => row.text).join("")).split("\n").filter((line) => line.trim());
  return lines.at(-1) ?? null;
};

export function BuildLogs({ steps, output, hasBuild, finished, now = Date.now() }: {
  steps: readonly BuildStepRow[]; output: readonly BuildOutputRow[]; hasBuild: boolean; finished: boolean; now?: number;
}) {
  const [opened, setOpened] = useState<ReadonlySet<number>>(new Set());
  const started = steps.filter((step) => step.startedAt !== null);
  if (!started.length) {
    return <p className="text-muted-foreground">{!hasBuild ? "This deployment uses prebuilt images. No build logs were produced." : finished ? "No retained build output for this deployment." : "Waiting for the build to start"}</p>;
  }
  const outputByStep = new Map<number, BuildOutputRow[]>();
  for (const row of output) outputByStep.set(row.stepId, [...outputByStep.get(row.stepId) ?? [], row]);
  return <ol>
    {started.map((step) => {
      const { stage, title } = splitStepName(step.name);
      const lines = outputByStep.get(step.id) ?? [];
      const failed = step.error !== null;
      const running = !failed && step.completedAt === null;
      const open = failed || opened.has(step.id);
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
      if (!lines.length && !failed) return <li key={step.id}><div className={row}>{summary}</div></li>;
      return <li key={step.id} className={cn(failed && "border-l-2 border-destructive")}>
        <details open={open} onToggle={(event) => setOpened((previous) => {
          const next = new Set(previous);
          if (event.currentTarget.open) next.add(step.id); else next.delete(step.id);
          return next;
        })}>
          <summary className={cn(row, "cursor-pointer list-none hover:bg-muted/40 [&::-webkit-details-marker]:hidden")}>{summary}</summary>
          {lines.length ? <pre className="whitespace-pre-wrap break-words pl-24">{lines.map((line) => <span key={line.id} className={line.stderr ? "text-foreground" : "text-muted-foreground"}>{stripAnsi(line.text)}</span>)}</pre> : null}
          {step.error ? <p className="whitespace-pre-wrap break-words pl-24 text-destructive">{step.error}</p> : null}
        </details>
        {tail ? <pre className="truncate pl-24 text-muted-foreground">{tail}</pre> : null}
      </li>;
    })}
  </ol>;
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
  const build = useBuildLog(organizationSlug, deploymentId, tab === "Build logs");
  const now = useNow(tab === "Build logs" && build.data?.finished === false);
  const logs = lifecycleLogs(events, serviceId);
  return <div className="rounded-lg bg-background p-4">
    <div className="mb-3 flex items-center gap-4">{(["Build logs", "Deploy logs"] as const).map((t) => <button key={t} type="button" className={cn("text-xs underline-offset-8", tab === t ? "underline" : "text-muted-foreground")} aria-pressed={tab === t} onClick={() => setTab(t)}>{t}</button>)}</div>
    {request.isError ? <p role="alert">Could not load deployment logs. <Button variant="ghost" size="sm" disabled={request.isFetching} onClick={() => void collection.utils.refetch()}>Retry</Button></p> : null}
    {build.isError ? <p role="alert">Could not load build logs. <Button variant="ghost" size="sm" disabled={build.isFetching} onClick={() => void build.refetch()}>Retry</Button></p> : null}
    {tab === "Deploy logs" ? <ContainerLogs selection={{ organizationSlug, deploymentId, serviceId }} lifecycle={logs} /> : <BuildLogViewer key={`${deploymentId}:${serviceId ?? "all"}`}>
      {build.isPending ? <p>Loading logs…</p> : <BuildLogs steps={build.data?.steps ?? []} output={build.data?.output ?? []} hasBuild={hasBuild} finished={build.data?.finished ?? true} now={now} />}
    </BuildLogViewer>}
  </div>;
}
