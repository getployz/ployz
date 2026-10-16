import { Suspense, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useParams } from "@tanstack/react-router";
import type { DeploymentView, ServerRow } from "@ployz/sdk";
import { ContainerLogs } from "#/components/container-logs";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { LatestButton, LogEmpty } from "#/components/log-scroll";
import { Skeleton } from "#/components/ui/skeleton";
import { cn } from "#/lib/utils";
import { missingDeployLogs, nodeStatusLabels, outcomeReason } from "#/modules/config-store/store-deployments";
import { buildLogQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { buildStatusLabels, rowLabel, rowLight, rowTiming, type OutcomeRunNode, type RunNode } from "./run-graph";
import { RunTime, runNodeIcons, runNodeKinds } from "./RunCanvas";

/** The picked node: its status, each Server's row as reported, then its logs. */
export function RunPanel({ deployment, node }: { deployment: DeploymentView; node: RunNode | undefined }) {
  return (
    <aside aria-label="Run node" className="flex min-h-0 flex-col overflow-hidden rounded-xl border bg-card text-card-foreground shadow-sm @max-5xl:min-h-full @max-5xl:shrink-0 @5xl:w-md @5xl:shrink-0">
      {node ? <RunNodeDetail key={node.id} deployment={deployment} node={node} /> : (
        <LogEmpty title="Nothing in this deployment" />
      )}
    </aside>
  );
}

function RunNodeDetail({ deployment, node }: { deployment: DeploymentView; node: RunNode }) {
  const Icon = runNodeIcons[node.type];
  const status = node.type === "build" ? buildStatusLabels[node.build.status] : nodeStatusLabels[node.node.outcome];
  const deleted = node.type === "volume" && node.node.outcome === "removed";
  return (
    <>
      <div className="flex shrink-0 flex-col gap-1 border-b p-4">
        <div className="flex min-w-0 items-center gap-2 [&_svg]:size-4">
          <DeploymentStatusIcon status={node.light} />
          <h2 className="truncate font-medium">{node.name}</h2>
          <span className={cn("text-sm", deleted ? "text-destructive" : "text-muted-foreground")}>{deleted ? "Deleted" : status}</span>
          <span className="ml-auto text-sm"><RunTime timing={node.timing} /></span>
        </div>
        <p className="flex items-center gap-1.5 text-sm text-muted-foreground [&_svg]:size-3.5">
          <Icon aria-hidden />{runNodeKinds[node.type]}
          {node.type === "build" && node.build.commit ? ` · ${node.build.commit.slice(0, 7)}` : null}
        </p>
      </div>
      {node.type === "build" ? <BuildDetail deployment={deployment} build={node.build} /> : <OutcomeDetail deployment={deployment} node={node} />}
    </>
  );
}

function BuildDetail({ deployment, build }: { deployment: DeploymentView; build: DeploymentView["builds"][number] }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2 p-4">
      {/* Said once: the header's reason already carries a failed build's message. */}
      {build.message && !outcomeReason(deployment.outcome)?.includes(build.message) ? <p className="break-words text-destructive">{build.message}</p> : null}
      <Suspense fallback={<Skeleton className="h-24 w-full" />}>
        <StoreBuildLog deploymentId={deployment.id} service={build.service} />
      </Suspense>
    </div>
  );
}

/** A Service's build output in the Deployment, as its Builder reported it; the change stream brings each new chunk. */
function StoreBuildLog({ deploymentId, service }: { deploymentId: string; service: string }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const result = useStoreView(organizationSlug, buildLogQuery(deploymentId, service));
  const element = useRef<HTMLPreElement>(null);
  const [following, setFollowing] = useState(true);
  const log = result.ok ? result.value.log : "";
  useLayoutEffect(() => {
    if (following && element.current) element.current.scrollTop = element.current.scrollHeight;
  }, [log, following]);
  useEffect(() => {
    const pre = element.current;
    if (!following || !pre) return;
    // The log keeps its end in view when the panel around it shrinks.
    const observer = new ResizeObserver(() => { pre.scrollTop = pre.scrollHeight; });
    observer.observe(pre);
    return () => observer.disconnect();
  }, [following, result.ok]);
  if (!result.ok) return <p className="text-muted-foreground">{result.refusal.message}</p>;
  return (
    <div className="relative flex min-h-0 flex-1 flex-col @max-5xl:h-96 @max-5xl:flex-none">
      <pre ref={element} aria-label={`${service} build output`} tabIndex={0}
        onScroll={({ currentTarget: pre }) => setFollowing(pre.scrollHeight - pre.scrollTop - pre.clientHeight <= 48)}
        className="min-h-0 flex-1 overflow-auto font-mono text-xs leading-6 break-words whitespace-pre-wrap">
        {log || "No output yet."}
      </pre>
      {following ? null : <LatestButton onClick={() => setFollowing(true)} />}
    </div>
  );
}

function OutcomeDetail({ deployment, node }: { deployment: DeploymentView; node: OutcomeRunNode }) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const missing = missingDeployLogs(deployment, node.node);
  return (
    <>
      {node.node.rows.length ? (
        <ul aria-label="Servers" className={cn("flex flex-col divide-y overflow-y-auto border-b", node.type === "service" ? "max-h-48 shrink-0" : "min-h-0")}>
          {node.node.rows.map((row) => <Row key={row.machine_id} row={row} />)}
        </ul>
      ) : node.type === "service" ? null : (
        <div className="p-4"><LogEmpty title={missing ?? "No servers"} /></div>
      )}
      {node.type === "service" ? (
        // Stacked, the logs get a fixed height so they scroll and follow new lines themselves.
        <div className="flex min-h-0 flex-1 flex-col p-4 @max-5xl:h-96 @max-5xl:flex-none">
          {missing ? <LogEmpty title={missing} />
            : <ContainerLogs selection={{ organizationSlug, deploymentId: deployment.id, serviceId: node.id }} />}
        </div>
      ) : null}
    </>
  );
}

function Row({ row }: { row: ServerRow }) {
  return (
    <li className="flex flex-col gap-1 px-4 py-2 text-sm">
      <div className="flex min-w-0 items-center gap-2 [&_svg]:size-3.5">
        <DeploymentStatusIcon status={rowLight(row)} />
        <span className="truncate">{row.server}</span>
        <span className="text-muted-foreground">{rowLabel(row)}</span>
        <span className="ml-auto"><RunTime timing={rowTiming([row])} /></span>
      </div>
      {row.state === "failed" ? (
        <div className="flex flex-col gap-0.5 pl-5.5 text-destructive">
          <p className="break-words">{row.reason}</p>
          {row.cause.map((cause) => <p key={cause} className="break-words text-muted-foreground">{cause}</p>)}
        </div>
      ) : null}
    </li>
  );
}
