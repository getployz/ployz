import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import type { DeploymentStatus } from "@ployz/sdk";
import type { ToolCallPart, ToolResultPart } from "@tanstack/ai-client";
import { Option, Schema } from "effect";
import { CircleCheckIcon, CircleDotIcon, CircleSlashIcon, CircleXIcon } from "lucide-react";
import type { CollectionScope } from "#/collections/scope";
import { DeploymentStatusIcon } from "#/components/deployment-status-icon";
import { Marker, MarkerContent, MarkerIcon } from "#/components/ui/marker";
import { deploymentStatusIcons, deploymentStatusLabel, deploysLabel } from "#/modules/config-store/store-deployments";
import { deploymentQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { DEPLOYMENT_PAGE_ROUTE_TO } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployment-page";
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
const Refused = Schema.Struct({
  ok: Schema.Literal(false),
  refusal: Schema.Struct({
    code: Schema.String,
    message: Schema.String,
    details: Schema.optional(Schema.Struct({ approval: Schema.optional(Schema.Struct({ reason: Schema.NullOr(Schema.String) })) })),
  }),
});
const Cancelled = Schema.Struct({ ok: Schema.Literal(false), cancelled: Schema.Literal(true) });
const Outcome = Schema.Union([Deployed, Refused, Cancelled]);
type Outcome = typeof Outcome.Type;
type Deployment = typeof Deployed.Type["value"];

/** A tool's answer as the sidebar draws it: the call's own output, or the result part's JSON text after a reload. */
export function toolOutcome(call: ToolCallPart, result: ToolResultPart | undefined): Option.Option<Outcome> {
  const decoded = Schema.decodeUnknownOption(Outcome)(call.output);
  if (Option.isSome(decoded) || result === undefined) return decoded;
  return Schema.decodeUnknownOption(Schema.fromJsonString(Outcome))(result.content);
}

/**
 * One tool call, compact: a Deployment it started as a Run card, a denial with its reason, a refusal with its message,
 * anything else as its command.
 */
export function ToolRow({ organizationSlug, scope, name, outcome, done }: {
  organizationSlug: string;
  scope: CollectionScope;
  name: string;
  outcome: Option.Option<Outcome>;
  done: boolean;
}) {
  const command = name.replaceAll("_", " ");
  if (Option.isNone(outcome)) {
    return (
      <Marker>
        <MarkerIcon>{done ? <CircleCheckIcon /> : <CircleDotIcon className="text-info" />}</MarkerIcon>
        <MarkerContent><span className="font-mono text-xs">ployz {command}</span></MarkerContent>
      </Marker>
    );
  }
  const answer = outcome.value;
  if ("value" in answer) {
    return (
      <div className="flex flex-col gap-1">
        <RunCard organizationSlug={organizationSlug} scope={scope} deployment={answer.value} />
        {answer.nothing_destroyed && <p className="text-xs text-muted-foreground">Didn't ask. Nothing destroyed.</p>}
      </div>
    );
  }
  if ("cancelled" in answer) {
    return <Marker><MarkerIcon><CircleSlashIcon /></MarkerIcon><MarkerContent>ployz {command} was cancelled.</MarkerContent></Marker>;
  }
  const { code, message, details } = answer.refusal;
  const denied = code === "approval_denied";
  return (
    <Marker>
      <MarkerIcon><CircleXIcon className="text-destructive" /></MarkerIcon>
      <MarkerContent>{denied ? `Denied${details?.approval?.reason ? `: ${details.approval.reason}` : "."}` : message}</MarkerContent>
    </Marker>
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
          ? <Link to={DEPLOYMENT_PAGE_ROUTE_TO} className="underline-offset-3 hover:underline"
              params={{ organizationSlug, projectSlug: live.environment.project, environmentSlug: live.environment.name, deploymentId: live.id }}>{title}</Link>
          : title}
        {deploysLabel(shown) && <span className="text-muted-foreground"> · {deploysLabel(shown)}</span>}
      </span>
      {ran && <span className="text-muted-foreground tabular-nums">{ran}</span>}
    </div>
  );
}
