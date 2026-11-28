import type { ConfigCommand, DestructiveEffect, EnvironmentRef, JsonValue } from "@ployz/sdk";
import { Schema } from "effect";
import { asRecord, asString } from "#/lib/json";
import { destroyedLine, droppedLine, type Guarded, prunes, type StoreReview } from "#/modules/deploy-approval/review";
import { type OperationVerb, operationWords } from "#/modules/operations/asked";

export const APPROVAL_STATUSES = ["pending", "approved", "denied", "superseded"] as const;
export type ApprovalStatus = (typeof APPROVAL_STATUSES)[number];

/** An operation as a human approves it: its verb, what it acts on by name, and the preview its digest covers. */
export type OperationReview = { verb: OperationVerb; name: string; preview: JsonValue; subject?: string };

/** A stop or restart the dashboard asked for, run as its asker once a protector approves it. */
export type ServiceApply = { environmentId: string; service: string; verb: "stop" | "restart" };

/** A Volume resize the dashboard asked for, run as its asker once a protector approves it. */
export type VolumeApply = { volumeId: string; environment: EnvironmentRef; maximum_bytes: number };

/**
 * What a human approves: the Destructive Effects, and either the Review they come from as the Store refused it, or the
 * operation's preview. A dashboard ask into a Protected Environment also keeps what runs as its asker once approved.
 */
export type ApprovalReview =
  | (StoreReview & { apply?: ConfigCommand })
  | { effects: DestructiveEffect[]; operation: OperationReview; protected?: Guarded; apply?: ServiceApply | VolumeApply };

const capitalized = (word: string) => `${word.charAt(0).toUpperCase()}${word.slice(1).replaceAll("_", " ")}`;

/**
 * What an approval asks about, as verb and name: "Deploy production", "Save production" or "Drain fra-1". A Store
 * write also names its Project, which the card shows and a denial leaves out.
 */
export function approvalSubject(command: string, review: ApprovalReview) {
  if ("operation" in review) {
    const verb = operationWords(review.operation.verb)?.title ?? capitalized(review.operation.verb);
    return { verb, name: review.operation.name, project: null };
  }
  const verb = command === "publish" ? "Save" : command === "retry" ? (shutsDownAgain(review) ? "Shut down" : rollsBack(review) ? "Roll back" : "Redeploy") : command === "remove" ? "Remove"
    : command === "shut_down" ? "Shut down" : command === "sync" ? "Sync" : command === "take" ? "Take" : command === "cancel" ? "Cancel" : "Deploy";
  return { verb, name: review.diff.environment.name, project: review.diff.environment.project };
}

/**
 * The restart or stop an approval card shows, as Servers know it: the verb, the Environment and Service from the ask, and
 * the Namespace and runtime name its approvals carry. Null for anything else, or an ask made before its preview named them.
 */
export function shownServiceAct(review: ApprovalReview) {
  if (!("operation" in review)) return null;
  const verb = review.operation.verb;
  if (verb !== "restart" && verb !== "stop") return null;
  const preview = asRecord(review.operation.preview);
  const environment = asString(preview?.["environment"]);
  const service = asString(preview?.["service"]);
  const namespace = asString(preview?.["namespace"]);
  const runtime = asString(preview?.["runtime"]);
  const only = asString(asRecord(preview?.["only"])?.["machine"]);
  if (environment === null || service === null || namespace === null || runtime === null) return null;
  return { verb, environment, service, namespace, runtime, only: only === null ? null : { machine: only } };
}

/** An approval's verb and name for a list, a Store write's Environment under its Project: "Deploy" "shop/production". */
export function approvalTarget(command: string, review: ApprovalReview) {
  const { verb, name, project } = approvalSubject(command, review);
  return { verb, name: project === null ? name : `${project}/${name}` };
}

/** A retry the Store refused without a `redeploy` is a removal's: it takes the Environment off again. */
export const shutsDownAgain = (review: ApprovalReview) => "diff" in review && !review.redeploy;

export const rollsBack = (review: ApprovalReview) => "diff" in review && (review.redeploy?.rolls_back.length ?? 0) > 0;

/**
 * Every line the card lists under what the plan destroys, as `ployz org approval show` prints them: each effect, then a
 * pruning Deploy's removal of the other Services, with the Servers it couldn't check.
 */
export function destroyedLines(command: string, review: ApprovalReview) {
  const lines = review.effects.map((effect) => {
    const { verb, name, after } = destroyedLine(effect, command);
    return `${verb}${name === null ? "" : ` ${name}`}${after}`;
  });
  if (!prunes(review)) return lines;
  const dropped = droppedLine(review);
  return [...lines, `Also removes any other service running in this environment${dropped === null ? "" : `. ${dropped}`}`];
}

const RenamePreview = Schema.Struct({ from: Schema.NullOr(Schema.String), to: Schema.String });

/** The private addresses a Cluster rename moves, as the card shows them; null for anything else. */
export function renamedAddresses(review: ApprovalReview) {
  if (!("operation" in review) || review.operation.verb !== "set_cluster_name") return null;
  const preview = Schema.decodeUnknownOption(RenamePreview)(review.operation.preview);
  if (preview._tag === "None") return null;
  const { from, to } = preview.value;
  return { from: from === null ? null : `*.${from}.ployz.internal`, to: `*.${to}.ployz.internal` };
}

const SwitchPreview = Schema.Struct({
  to: Schema.Literals(["tunnel", "public"]),
  zone: Schema.NullOr(Schema.String),
  hostedName: Schema.NullOr(Schema.String),
});

/** The addresses a switch of web traffic moves Services between, as the card shows them; null for anything else. */
export function switchedAddresses(review: ApprovalReview) {
  if (!("operation" in review) || review.operation.verb !== "set_ingress_doors") return null;
  const preview = Schema.decodeUnknownOption(SwitchPreview)(review.operation.preview);
  if (preview._tag === "None") return null;
  const { to, zone, hostedName } = preview.value;
  const [from, onto] = to === "tunnel" ? [hostedName, zone] : [zone, hostedName];
  return from === null || onto === null ? null : { from: `*.${from}`, to: `*.${onto}` };
}

export const asksForAuthKey = (review: ApprovalReview) => "operation" in review
  && review.operation.verb === "set_private_access" && review.operation.name === "on";

/** `POST /api/cli/approvals/:id`: approve exactly the digest the human saw, or deny with a reason the agent gets back. */
export const ApprovalDecision = Schema.Union([
  Schema.Struct({ approve: Schema.Struct({ digest: Schema.String.check(Schema.isNonEmpty()) }) }),
  Schema.Struct({ reject: Schema.Struct({ reason: Schema.optional(Schema.String.check(Schema.isMaxLength(1024))) }) }),
]);
export type ApprovalDecision = typeof ApprovalDecision.Type;

export { destroyedLine, droppedLine, prunes };


/** A Store write as an approval names it: a retry apart from the Deploy it ships again. */
export function approvalCommand(command: ConfigCommand) {
  if (command.command === "admit" && (command.admit === "retry" || command.admit === "shut_down")) return command.admit;
  return command.command;
}
