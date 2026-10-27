import type { Asks, ConfigCommand, DestructiveEffect, DestructiveKind, DiffView, EnvironmentRef, JsonValue } from "@ployz/sdk";
import { Schema } from "effect";
import type { Caller } from "#/modules/identity/actor";
import { rollbackConfirm } from "#/modules/config-store/store-deployments";

export const APPROVAL_STATUSES = ["pending", "approved", "denied", "superseded"] as const;
export type ApprovalStatus = (typeof APPROVAL_STATUSES)[number];

/** The operations `ployz` and agents run through Cloud that may wait for a human. Upgrade and a Service start never do. */
export type OperationVerb = "drain" | "set" | "clean" | "remove" | "forget" | "release" | "remove_mirror" | "stop" | "restart" | "unprotect" | "move" | "restore" | "resize";

/** An operation as a human approves it: its verb, what it acts on by name, and the preview its digest covers. */
export type OperationReview = { verb: OperationVerb; name: string; preview: JsonValue; subject?: string };

/** A stop or restart the dashboard asked for, run as its asker once a protector approves it. */
export type ServiceApply = { environmentId: string; service: string; verb: "stop" | "restart" };

/** A Volume resize the dashboard asked for, run as its asker once a protector approves it. */
export type VolumeApply = { volumeId: string; environment: EnvironmentRef; maximum_bytes: number };

/**
 * An ask into the Protected Environment `environment` by someone who can't protect it: someone who can, other than its
 * asker, decides it.
 */
export type Guarded = { environment: string };

/** A redeploy as its approval binds it: the Deployment it ships again, by number, and the newer ones it rolls back. */
export type Redeploy = { deployment: number; rolls_back: number[] };

/**
 * What a human approves: the Destructive Effects, and either the Review they come from as the Store refused it, or the
 * operation's preview. A dashboard ask into a Protected Environment also keeps what runs as its asker once approved.
 */
export type ApprovalReview =
  | { effects: DestructiveEffect[]; diff: DiffView; protected?: Guarded; apply?: ConfigCommand; redeploy?: Redeploy; prunes?: true; dropped?: string[] }
  | { effects: DestructiveEffect[]; operation: OperationReview; protected?: Guarded; apply?: ServiceApply | VolumeApply };

const capitalized = (word: string) => `${word.charAt(0).toUpperCase()}${word.slice(1).replaceAll("_", " ")}`;

/**
 * What an approval asks about, as verb and name: "Deploy production", "Save production" or "Drain fra-1". A Store
 * write also names its Project, which the card shows and a denial leaves out.
 */
export function approvalSubject(command: string, review: ApprovalReview) {
  if ("operation" in review) return { verb: capitalized(review.operation.verb), name: review.operation.name, project: null };
  const verb = command === "publish" ? "Save" : command === "retry" ? (shutsDownAgain(review) ? "Shut down" : rollsBack(review) ? "Roll back" : "Redeploy") : command === "remove" ? "Remove"
    : command === "shut_down" ? "Shut down" : command === "sync" ? "Sync" : command === "take" ? "Take" : command === "cancel" ? "Cancel" : "Deploy";
  return { verb, name: review.diff.environment.name, project: review.diff.environment.project };
}

/** An approval's verb and name for a list, a Store write's Environment under its Project: "Deploy" "shop/production". */
export function approvalTarget(command: string, review: ApprovalReview) {
  const { verb, name, project } = approvalSubject(command, review);
  return { verb, name: project === null ? name : `${project}/${name}` };
}

/** A retry the Store refused without a `redeploy` is a removal's: it takes the Environment off again. */
const shutsDownAgain = (review: ApprovalReview) => "diff" in review && !review.redeploy;

const rollsBack = (review: ApprovalReview) => "diff" in review && (review.redeploy?.rolls_back.length ?? 0) > 0;

/** What a redeploy's card says it ships: "Back to #4. Undoes #5 and #6.", or "Ships #4 again."; null for anything else. */
export function redeployLine(command: string, review: ApprovalReview) {
  if (command !== "retry" || !("diff" in review) || !review.redeploy) return null;
  const { deployment, rolls_back } = review.redeploy;
  const rollback = rollbackConfirm({ number: deployment, rolls_back });
  return rollback ? `Back to #${deployment}. ${rollback.body}.` : `Ships #${deployment} again.`;
}

/** A full Deploy that also removes every other Service its Environment runs, which only the Servers can name. */
export const prunes = (review: ApprovalReview) => "diff" in review && review.prunes === true;

/** Why a pruning Deploy couldn't check every Server: the ones large packets don't reach. Null when it doesn't say. */
export function droppedLine(review: ApprovalReview) {
  if (!prunes(review) || !("diff" in review) || (review.dropped?.length ?? 0) === 0) return null;
  return `Couldn't check every server: large packets to ${review.dropped?.join(", ")} are dropped on the mesh path`;
}

/**
 * Every line the card lists under what the plan destroys, as `ployz org approval show` prints them: each effect, then a
 * pruning Deploy's removal of the other Services, with the Servers it couldn't check.
 */
export function destroyedLines(command: string, review: ApprovalReview) {
  const lines = review.effects.map((effect) => {
    const { verb, name, after } = destroyedLine(effect, command, review);
    return `${verb}${name === null ? "" : ` ${name}`}${after}`;
  });
  if (!prunes(review)) return lines;
  const dropped = droppedLine(review);
  return [...lines, `Also removes any other service running in this environment${dropped === null ? "" : `. ${dropped}`}`];
}

/** A Server removal: its Volumes' data stays on the disk, so the card lists effects rather than destruction. */
export const losesVolumeAccess = (review?: ApprovalReview) => review !== undefined && "operation" in review
  && review.operation.verb === "remove" && review.operation.subject?.startsWith("server:") === true;

/** The card's badge: a Save deletes nothing until the next Deploy, and a Deploy that only deletes Volumes counts them. An agent's Deploy that destroys nothing still asks. */
export function approvalBadge(command: string, review: ApprovalReview) {
  const { effects } = review;
  if (losesVolumeAccess(review)) return "Removes server";
  if (command === "publish") return "Deletes on deploy";
  if (command === "retry" && !shutsDownAgain(review)) return rollsBack(review) ? "Rolls back" : "Redeploys";
  if (command === "stop") return "Stops service";
  if (command === "restart") return "Restarts service";
  if (command === "unprotect") return "Unprotects";
  if (command === "sync") return "Syncs";
  if (command === "take") return "Takes";
  if (command === "cancel") return "Cancels deploy";
  if (command === "move") return "Moves volume";
  if (command === "restore") return "Restores volume";
  if (command === "resize") return "Resizes volume";
  if (command === "release" && effects.length === 0) return "Releases volume";
  if (effects.length === 0) return prunes(review) ? "Removes other services" : "Deploys";
  if (effects.every((effect) => effect.kind === "deletes_volume")) {
    return `Deletes ${effects.length} ${effects.length === 1 ? "volume" : "volumes"}`;
  }
  return `Destroys ${effects.length} ${effects.length === 1 ? "thing" : "things"}`;
}

/** A Store write as an approval names it: a retry apart from the Deploy it ships again. */
export function approvalCommand(command: ConfigCommand) {
  if (command.command === "admit" && (command.admit === "retry" || command.admit === "shut_down")) return command.admit;
  return command.command;
}

const agentShips = (via: Caller["via"], command: ConfigCommand) =>
  via === "agent" && command.command === "admit" && (command.admit === "deploy" || command.admit === "retry");

/**
 * Cloud can't tell a person running `ployz` from an agent running it, so only a dashboard confirm stands as the approval.
 * A retry counts, since a shutdown's retry or a rollback takes Services off, and so does a cancel, which a member asks
 * an admin for in a Protected Environment. The Store decides which of them ask.
 */
const askedRemoval = (via: Caller["via"], command: ConfigCommand) =>
  via !== "dashboard" && (command.command === "remove" || command.command === "cancel"
    || (command.command === "admit" && (command.admit === "shut_down" || command.admit === "retry")));

/**
 * The writes that may wait for a human: a Save (the Store's publish), a manual Deploy, a Sync or Take (into a Protected
 * Environment), a retry, a cancel, and a Project or Environment removal or shutdown from `ployz` or an agent.
 */
export function asksApproval(via: Caller["via"], command: ConfigCommand) {
  return command.command === "publish" || (command.command === "admit" && command.admit === "deploy")
    || command.command === "sync" || command.command === "take"
    || agentShips(via, command) || askedRemoval(via, command);
}

/** Which Deploys wait for a human: every Deploy or retry an agent asks for, else only a Deploy that destroys something. */
export function approvalAsks(via: Caller["via"], command: ConfigCommand): Asks {
  return agentShips(via, command) ? "every_deploy" : "destroys";
}

/** `POST /api/cli/approvals/:id`: approve exactly the digest the human saw, or deny with a reason the agent gets back. */
export const ApprovalDecision = Schema.Union([
  Schema.Struct({ approve: Schema.Struct({ digest: Schema.String.check(Schema.isNonEmpty()) }) }),
  Schema.Struct({ reject: Schema.Struct({ reason: Schema.optional(Schema.String.check(Schema.isMaxLength(1024))) }) }),
]);
export type ApprovalDecision = typeof ApprovalDecision.Type;

const destroys = {
  removes_service: { verb: "Removes", noun: "service", after: "" },
  deletes_volume: { verb: "Deletes", noun: "volume", after: ". Not recoverable." },
  detaches_volume: { verb: "Detaches", noun: "volume", after: "" },
  removes_domain: { verb: "Removes", noun: "domain", after: "" },
  removes_server: { verb: "Removes", noun: "server", after: "" },
  removes_member: { verb: "Removes", noun: "member", after: "" },
  changes_role: { verb: "Changes", noun: "role of", after: "" },
  revokes_invite: { verb: "Revokes", noun: "invite for", after: "" },
  stops_service: { verb: "Stops", noun: "service", after: " until started again" },
  restarts_service: { verb: "Restarts", noun: "service", after: "" },
  unprotects_environment: { verb: "Unprotects", noun: "environment", after: "" },
  removes_organization: { verb: "Removes", noun: "organization", after: ". Not recoverable." },
} satisfies Record<DestructiveKind, { verb: string; noun: string; after: string }>;

/**
 * One thing the plan destroys, worded as the card leads with it: by name, or as "a volume" when it has none. A Save
 * destroys nothing itself, so its lines say the next Deploy does.
 */
export function destroyedLine(effect: DestructiveEffect, command = "admit", review?: ApprovalReview) {
  if (effect.kind === "deletes_volume" && losesVolumeAccess(review)) {
    return {
      verb: effect.name === undefined ? "Loses access to a volume" : "Loses access to volume",
      name: effect.name ?? null,
      after: ". Its data stays on the Server's disk until wiped. Wipe or reinstall the disk to erase it; sudo ployz-uninstall keeps the data.",
    };
  }
  const { verb: does, noun, after } = destroys[effect.kind];
  const save = command === "publish";
  const verb = save ? `Next deploy ${does.toLowerCase()}` : does;
  const end = save ? "." : after;
  const mount = effect.kind === "detaches_volume" ? effect.path.split(".mounts.") : [];
  const tail = mount.length === 2 ? ` from ${mount[0]}${end}` : end;
  return effect.name === undefined ? { verb: `${verb} a ${noun}`, name: null, after: tail } : { verb: `${verb} ${noun}`, name: effect.name, after: tail };
}
