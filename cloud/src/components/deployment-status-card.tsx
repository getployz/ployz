import { useId, type ReactNode } from "react";
import { CheckIcon, ChevronDownIcon, CircleIcon, MinusIcon, TriangleAlertIcon, XIcon } from "lucide-react";
import { Spinner } from "#/components/ui/spinner";
import { Button } from "#/components/ui/button";
import { cn } from "#/lib/utils";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import { progressRowLabel, type DeploymentProgress, type DeploymentProgressRow } from "#/modules/deployments/deployment-progress";
import { formatRelativeTime } from "#/utils/relative-time";

type StepState = "pending" | "running" | "completed" | "failed" | "skipped" | "unknown";
function Step({ title, detail, state, children }: { title: string; detail: string; state: StepState; children?: ReactNode }) {
  const Icon = state === "completed" ? CheckIcon : state === "failed" ? XIcon : state === "skipped" ? MinusIcon : state === "unknown" ? TriangleAlertIcon : CircleIcon;
  return <div className={cn("px-4 py-3 sm:px-6", state === "failed" && "bg-destructive/8 text-destructive")}>
    <div className="flex items-start gap-3 sm:gap-5">
      {state === "running" ? <Spinner className="mt-0.5 size-4 shrink-0 motion-reduce:animate-none" /> : <Icon className={cn("mt-0.5 size-4 shrink-0", state === "completed" ? "text-success" : state === "failed" ? "text-destructive" : "text-muted-foreground")} />}
      <div className="min-w-0 flex-1 text-sm"><span className="font-medium">{title}</span><span className={cn("ml-1", state !== "failed" && "text-muted-foreground")}> › {detail}</span>{children}</div>
      {state === "pending" || state === "skipped" ? <span className="shrink-0 text-xs text-muted-foreground">{state === "pending" ? "Not started" : "Skipped"}</span> : null}
    </div>
  </div>;
}
export function summarizeRows(rows: readonly DeploymentProgressRow[]) {
  const replicas = rows.filter((r) => r.operation === "replace_container" || r.operation === "run_container");
  const done = rows.filter((r) => r.status === "completed").length;
  const failed = rows.filter((r) => r.status === "failed").length;
  const running = rows.filter((r) => r.status === "running").length;
  const unexecuted = rows.filter((r) => r.status === "unexecuted").length;
  return [replicas.length ? `${replicas.filter((r) => r.status === "completed").length} / ${replicas.length} replicas updated` : `${done} / ${rows.length} operations complete`, running ? `${running} in progress` : null, failed ? `${failed} failed` : null, unexecuted ? `${unexecuted} not attempted` : null].filter(Boolean).join(" · ");
}

export function DeploymentStatusCard({ deployment, progress, logsPanel, showLogs, onLogsChange, expanded, onExpandedChange, actions, children, serviceId }: {
  deployment: EnvironmentDeploymentSummary; progress: DeploymentProgress | null;
  logsPanel: ReactNode; showLogs: boolean; onLogsChange: (open: boolean) => void; expanded: boolean; onExpandedChange: (open: boolean) => void;
  actions: ReactNode; children?: ReactNode; serviceId?: string;
}) {
  const id = useId();
  const rows = progress?.rows.filter((r) => !serviceId || r.serviceId === serviceId) ?? [];
  const failedRows = rows.filter((r) => r.status === "failed");
  const active = ["queued", "planning", "deploying"].includes(deployment.status);
  const cancelling = active && Boolean(deployment.cancellationRequestedAt);
  const partial = progress?.outcome === "failed" && rows.some((r) => r.status === "completed") && rows.some((r) => r.status !== "completed");
  const notAttempted = Boolean(serviceId && progress?.outcome === "failed" && rows.length && rows.every((r) => r.status === "unexecuted"));
  const unchanged = Boolean(serviceId && progress?.outcome && rows.length === 0);
  const serviceDone = Boolean(serviceId && progress?.outcome && rows.length && rows.every((r) => r.status === "completed"));
  const successful = serviceDone || (!serviceId && (progress?.outcome === "success" || deployment.status === "applied"));
  const postDone = deployment.status === "applied" || (serviceDone && !active);
  const failed = failedRows.length > 0 || (!serviceDone && deployment.status === "failed");
  const badge = cancelling ? "Cancelling" : unchanged ? "Unchanged" : serviceDone ? "Applied" : notAttempted ? "Not attempted" : partial ? "Partial" : deployment.status === "queued" ? "Queued" : deployment.status === "planning" ? "Initializing" : deployment.status;
  const tone = failed || partial ? "border-destructive/30 bg-destructive/4" : successful ? "border-success/30 bg-success/4" : active ? "border-info/30 bg-info/4" : "border-border bg-muted/20";
  const accent = failed || partial ? "text-destructive" : successful ? "text-success" : active ? "text-info" : "text-muted-foreground";
  const unknown = !active && deployment.status !== "applied" && !progress?.outcome && deployment.failureCode === "sdk_deploy_outcome_unknown";
  const buildFailed = deployment.failureCode === "deploy_image_not_pullable";
  const initState: StepState = progress || deployment.deployPreview || deployment.status === "applied" ? "completed" : deployment.status === "planning" ? "running" : deployment.status === "failed" && !buildFailed ? "failed" : "pending";
  const deployState: StepState = unchanged ? "skipped" : failedRows.length ? "failed" : successful ? "completed" : unknown ? "unknown" : deployment.status === "deploying" ? "running" : progress?.outcome === "failed" ? "failed" : "pending";
  const runningLabels = [...new Set(rows.filter((r) => r.status === "running").map(progressRowLabel))];
  const serviceGroups = new Map<string, DeploymentProgressRow[]>();
  for (const row of rows) {
    const key = row.serviceId ?? row.serviceName ?? "Environment";
    const group = serviceGroups.get(key) ?? [];
    group.push(row);
    serviceGroups.set(key, group);
  }
  const headline = cancelling ? "Cancelling deployment · waiting for runtime cleanup" : unchanged ? "No changes for this service" : serviceDone ? "Service applied" : notAttempted ? "Service not attempted" : partial ? "Rollout partially applied" : successful ? "Deployment successful" : unknown ? "Deployment ended · runtime outcome unknown" : failed ? "Deployment failed" : deployment.status === "cancelled" ? "Deployment cancelled" : deployment.status === "queued" ? (deployment.dispatchRequestedAt ? "Waiting to deploy" : "Queued for next trigger") : deployment.status === "applied" ? "Environment applied · service evidence unavailable" : "Deployment in progress";
  return <article className={cn("min-w-0 rounded-xl border p-1", tone)}>
    <header className="flex flex-wrap items-center gap-3 rounded-lg px-3 py-4 sm:gap-5 sm:px-5">
      <span className={cn("rounded-md bg-muted/50 px-2.5 py-1.5 text-xs font-medium capitalize", accent)}>{badge}</span>
      <div className="min-w-0 flex-1"><p className="truncate text-sm font-medium">{serviceId ? `${rows[0]?.serviceName ?? "Service"} · ` : ""}{deployment.message ?? "Deployment"}</p><p className="mt-1 text-xs text-muted-foreground">{deployment.projectSlug} / {deployment.environmentSlug} · {formatRelativeTime(deployment.createdAt)}{!serviceId ? ` · ${deployment.serviceCount} ${deployment.serviceCount === 1 ? "service" : "services"}` : ""}</p></div>
      <Button variant="outline" size="sm" onClick={() => { onLogsChange(!showLogs); onExpandedChange(true); }} aria-expanded={showLogs} aria-controls={`${id}-logs`}>{showLogs ? "Hide logs" : "View logs"}</Button>{actions}
    </header>
    <button type="button" className={cn("flex w-full items-center gap-3 rounded-md bg-muted/30 px-4 py-3 text-left text-sm focus-visible:outline-2 focus-visible:outline-ring sm:px-6", accent)} aria-expanded={expanded} aria-controls={`${id}-steps`} onClick={() => onExpandedChange(!expanded)}>
      {successful ? <CheckIcon className="size-4" /> : failed || unknown ? <TriangleAlertIcon className="size-4" /> : active && deployment.status !== "queued" ? <Spinner className="size-4 motion-reduce:animate-none" /> : null}
      <span className="flex-1">{headline}</span><ChevronDownIcon className={cn("size-4", expanded && "rotate-180")} />
    </button>
    {expanded ? <div id={`${id}-steps`} className="rounded-b-lg bg-background py-2">
      <Step title="Init" state={initState} detail={initState === "completed" ? "Configuration and targets prepared" : initState === "running" ? "Resolving configuration and planning targets" : initState === "failed" ? deployment.failureMessage ?? "Planning failed" : "Waiting to start"} />
      <Step title="Build" state={buildFailed ? "failed" : "skipped"} detail={buildFailed ? "Image must be built before deployment" : "Using prebuilt images"} />
      <Step title="Deploy" state={deployState} detail={unchanged ? "No operations planned for this service" : rows.length ? summarizeRows(rows) : progress?.outcome === "success" ? "No runtime changes needed" : deployment.status === "applied" ? "Applied · operation evidence unavailable" : unknown ? "Runtime outcome unavailable" : deployment.status === "deploying" ? "Waiting for runtime progress" : "Waiting to start"}>
        {runningLabels.length ? <p className="mt-1 text-xs text-muted-foreground">{runningLabels.join(" · ")}</p> : null}
        {serviceGroups.size > 1 && !serviceId ? <div className="mt-3 space-y-2">{[...serviceGroups].map(([key, group]) => <p key={key} className="flex flex-wrap justify-between gap-2 text-xs"><span>{group[0]?.serviceName ?? "Environment"}</span><span className="text-muted-foreground">{summarizeRows(group)}</span></p>)}</div> : null}
        {rows.filter((r) => r.elapsedMs !== null && (r.status === "running" || r.status === "failed")).map((r) => <p key={r.index} className="mt-2 text-xs text-muted-foreground">{r.displayName ?? r.serviceName} · {r.machineName ?? r.machineId} · {r.health ?? progressRowLabel(r)} · {Math.floor((r.elapsedMs ?? 0) / 1000)}s / {Math.floor((r.deadlineMs ?? 0) / 1000)}s deadline</p>)}
        {failedRows.map((r) => <p key={r.index} className="mt-2 break-words font-mono text-xs">{r.serviceName} · {r.machineName ?? r.machineId} · {r.error}</p>)}
        {failed && !failedRows.length && deployment.failureMessage ? <p className="mt-2 break-words text-xs">{deployment.failureMessage}</p> : null}
        {progress?.compensation.length ? <div className="mt-3 border-l border-primary/30 pl-3 text-xs"><p className="font-medium">Recovery reported by runtime</p>{progress.compensation.map((line) => <p key={line} className="mt-1">{line}</p>)}</div> : null}
        {unknown ? <p className="mt-2 text-xs text-muted-foreground">A complete runtime outcome was not received. Completed operations remain recorded; remaining effects are unknown.</p> : null}
      </Step>
      <Step title="Post-deploy" state={postDone ? "completed" : progress?.outcome === "success" && active ? "running" : "pending"} detail={postDone ? "Applied state recorded" : "Record applied state"} />
      {deployment.deployPreview?.warnings.length ? <details className="mx-6 my-2 text-xs"><summary className="cursor-pointer text-warning">{deployment.deployPreview.warnings.length} planning warnings</summary>{deployment.deployPreview.warnings.map((w, i) => <p key={i} className="mt-2 break-words">{JSON.stringify(w)}</p>)}</details> : null}
      {children}
    </div> : null}
    {showLogs ? <section id={`${id}-logs`} className="mt-1">{logsPanel}</section> : null}
  </article>;
}
