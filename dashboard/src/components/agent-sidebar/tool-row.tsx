import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import type { DeploymentStatus, RunState } from "@ployz/sdk";
import type { ToolCallPart, ToolResultPart } from "@tanstack/ai-client";
import { Option, Schema } from "effect";
import { CircleCheckIcon, CircleSlashIcon, CircleXIcon } from "lucide-react";
import type { CollectionScope } from "#/collections/scope";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { Spinner } from "#/components/ui/spinner";
import { TraceDescription, TraceIcon, TraceLine, TraceTitle } from "#/components/ui/trace";
import { type DeploymentLight, deploymentStatusIcons, deploymentStatusLabel, deploysLabel } from "#/modules/config-store/store-deployments";
import { deploymentQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { deploymentDestination } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/run-search";
import { formatDuration } from "#/utils/relative-time";

const DEPLOYMENT_STATUSES = [
  "queued", "superseded", "running", "applied", "failed", "unknown", "cancelling", "cancelled",
] as const satisfies readonly DeploymentStatus[];

const Deployed = Schema.Struct({
  ok: Schema.Literal(true),
  value: Schema.Struct({
    written: Schema.Literal("deployment"),
    id: Schema.String,
    number: Schema.Number,
    status: Schema.Literals(DEPLOYMENT_STATUSES),
    services: Schema.Array(Schema.String),
    remove: Schema.Boolean,
    started_at: Schema.NullOr(Schema.Number),
    ended_at: Schema.NullOr(Schema.Number),
  }),
  nothing_destroyed: Schema.optional(Schema.Literal(true)),
});
const RUN_STATES = {
  accepted: { light: "queued", label: "Queued" },
  running: { light: "deploying", label: "Running" },
  done: { light: "deployed", label: "Done" },
  failed: { light: "failed", label: "Failed" },
  unknown: { light: "unknown", label: "Unknown" },
  superseded: { light: "cancelled", label: "Superseded" },
} as const satisfies Record<RunState, { light: DeploymentLight; label: string }>;

const Ran = Schema.Struct({
  ok: Schema.Literal(true),
  value: Schema.Struct({
    at: Schema.String,
    run: Schema.String,
    state: Schema.Literals(["accepted", "running", "done", "failed", "unknown", "superseded"] satisfies readonly RunState[]),
  }),
});
const isRan = Schema.is(Ran);
const Refused = Schema.Struct({
  ok: Schema.Literal(false),
  refusal: Schema.Struct({
    code: Schema.String,
    message: Schema.String,
    details: Schema.optional(Schema.NullOr(Schema.Struct({ approval: Schema.optional(Schema.Struct({ reason: Schema.NullOr(Schema.String) })) }))),
  }),
});
const Cancelled = Schema.Struct({ ok: Schema.Literal(false), cancelled: Schema.Literal(true) });
const Outcome = Schema.Union([Deployed, Ran, Refused, Cancelled]);
type Outcome = typeof Outcome.Type;
type Deployment = typeof Deployed.Type["value"];
type Run = typeof Ran.Type["value"];

/** A tool's answer as the sidebar draws it: the call's own output, or the result part's JSON text after a reload. */
export function toolOutcome(call: ToolCallPart, result: ToolResultPart | undefined): Option.Option<Outcome> {
  const decoded = Schema.decodeUnknownOption(Outcome)(call.output);
  if (Option.isSome(decoded) || result === undefined) return decoded;
  return Schema.decodeUnknownOption(Schema.fromJsonString(Outcome))(result.content);
}

/** One tool call as a Trace line: its command, ticked when done, dimmed when cancelled, red with its reason when refused. */
export function ToolStep({ name, outcome, done }: { name: string; outcome: Option.Option<Outcome>; done: boolean }) {
  const command = `ployz ${name.replaceAll("_", " ")}`;
  const answer = Option.getOrUndefined(outcome);
  if (answer === undefined || answer.ok) {
    return (
      <TraceLine status={done ? undefined : "running"}>
        <TraceIcon>{done ? <CircleCheckIcon /> : <Spinner />}</TraceIcon>
        <TraceTitle className="font-mono">{command}</TraceTitle>
      </TraceLine>
    );
  }
  if ("cancelled" in answer) {
    return (
      <TraceLine status="pending">
        <TraceIcon><CircleSlashIcon /></TraceIcon>
        <TraceTitle className="font-mono">{command}</TraceTitle>
        <TraceDescription>cancelled</TraceDescription>
      </TraceLine>
    );
  }
  return (
    <div className="flex flex-col gap-1">
      <TraceLine status="error">
        <TraceIcon><CircleXIcon /></TraceIcon>
        <TraceTitle className="font-mono">{command}</TraceTitle>
      </TraceLine>
      <p>{refusalReason(answer.refusal)}</p>
    </div>
  );
}

function refusalReason({ code, message, details }: typeof Refused.Type["refusal"]): string {
  return code === "approval_denied" ? `Denied${details?.approval?.reason ? `: ${details.approval.reason}` : "."}` : message;
}

/** The card a call leaves under the trace: a Deployment it started, or a Cloud run it started. */
export function ToolCard({ organizationSlug, scope, outcome }: { organizationSlug: string; scope: CollectionScope; outcome: Option.Option<Outcome> }) {
  const answer = Option.getOrUndefined(outcome);
  if (answer === undefined || !answer.ok) return null;
  if (isRan(answer)) return <OperationCard run={answer.value} />;
  return (
    <div className="flex flex-col gap-1">
      <RunCard organizationSlug={organizationSlug} scope={scope} deployment={answer.value} />
      {answer.nothing_destroyed && <p className="text-xs text-muted-foreground">Didn't ask. Nothing destroyed.</p>}
    </div>
  );
}

/** A Deployment the agent started, followed live: its status in the Deployment Page's words, how long it ran, and a way there. */
export function RunCard({ organizationSlug, scope, deployment }: { organizationSlug: string; scope: CollectionScope; deployment: Deployment }) {
  const { data } = useQuery(storeViewOptions(organizationSlug, scope, deploymentQuery(deployment.id)));
  const live = data?.ok ? data.value : null;
  const shown = live ?? { ...deployment, services: [...deployment.services], outcome: null };
  const ran = shown.started_at !== null && shown.ended_at !== null ? formatDuration(shown.ended_at - shown.started_at) : null;
  const title = `Deployment #${shown.number}`;
  return (
    <div className="flex items-center gap-2 rounded-lg border px-2.5 py-1.5 text-xs [&_svg]:size-4 [&_svg]:shrink-0">
      <DeploymentStatusIcon status={deploymentStatusIcons[shown.status]} />
      <span className="min-w-0 flex-1 truncate">
        <span className="font-medium">{deploymentStatusLabel(shown)}</span>
        {" · "}
        {live
          ? <Link className="underline-offset-3 hover:underline"
              {...deploymentDestination({ organizationSlug, projectSlug: live.environment.project, environmentSlug: live.environment.name }, live.id)}>{title}</Link>
          : title}
        {deploysLabel(shown) && <span className="text-muted-foreground"> · {deploysLabel(shown)}</span>}
      </span>
      {ran && <span className="text-muted-foreground tabular-nums">{ran}</span>}
    </div>
  );
}

export function OperationCard({ run }: { run: Run }) {
  const { light, label } = RUN_STATES[run.state];
  return (
    <div className="flex items-center gap-2 rounded-lg border px-2.5 py-1.5 text-xs [&_svg]:size-4 [&_svg]:shrink-0">
      <DeploymentStatusIcon status={light} />
      <span className="min-w-0 flex-1 truncate"><span className="font-medium">{label}</span>{" · "}<span>{run.run}</span></span>
      <span className="text-muted-foreground">{run.at}</span>
    </div>
  );
}
