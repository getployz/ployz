import type { ConfigCommand, DestructiveEffect, DestructiveKind, DiffView, JsonValue } from "@ployz/sdk";
import { Schema } from "effect";

export const APPROVAL_STATUSES = ["pending", "approved", "denied", "superseded"] as const;
export type ApprovalStatus = (typeof APPROVAL_STATUSES)[number];

/** The Server and Namespace operations the CLI runs through Cloud that may wait for a human. Upgrade never does. */
export type OperationVerb = "drain" | "set" | "clean" | "remove" | "forget" | "release" | "remove_mirror";

/** An operation as a human approves it: its verb, what it acts on by name, and the preview its digest covers. */
export type OperationReview = { verb: OperationVerb; name: string; preview: JsonValue };

/**
 * What a human approves: the Destructive Effects, and either the Review they come from as the Store refused it, or the
 * operation's preview.
 */
export type ApprovalReview =
  | { effects: DestructiveEffect[]; diff: DiffView }
  | { effects: DestructiveEffect[]; operation: OperationReview };

const capitalized = (word: string) => `${word.charAt(0).toUpperCase()}${word.slice(1).replaceAll("_", " ")}`;

/** What an approval asks about, as verb and name: "Deploy production", "Save production" or "Drain fra-1". */
export function approvalSubject(command: string, review: ApprovalReview) {
  if ("operation" in review) return { verb: capitalized(review.operation.verb), name: review.operation.name };
  return { verb: command === "publish" ? "Save" : "Deploy", name: review.diff.environment.name };
}

/** The card's badge: a Save deletes nothing until the next Deploy, and a Deploy that only deletes Volumes counts them. */
export function approvalBadge(command: string, { effects }: ApprovalReview) {
  if (command === "publish") return "Deletes on deploy";
  if (effects.length > 0 && effects.every((effect) => effect.kind === "deletes_volume")) {
    return `Deletes ${effects.length} ${effects.length === 1 ? "volume" : "volumes"}`;
  }
  return `Destroys ${effects.length} ${effects.length === 1 ? "thing" : "things"}`;
}

/** The writes the Store reviews for destruction: Save only (the Store's publish), and a manual Deploy. Nothing else waits for a human. */
export function asksApproval(command: ConfigCommand) {
  return command.command === "publish" || (command.command === "admit" && command.admit === "deploy");
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
} satisfies Record<DestructiveKind, { verb: string; noun: string; after: string }>;

/**
 * One thing the plan destroys, worded as the card leads with it: by name, or as "a volume" when it has none. A Save
 * destroys nothing itself, so its lines say the next Deploy does.
 */
export function destroyedLine(effect: DestructiveEffect, command = "admit") {
  const { verb: does, noun, after } = destroys[effect.kind];
  const save = command === "publish";
  const verb = save ? `Next deploy ${does.toLowerCase()}` : does;
  const end = save ? "." : after;
  const mount = effect.kind === "detaches_volume" ? effect.path.split(".mounts.") : [];
  const tail = mount.length === 2 ? ` from ${mount[0]}${end}` : end;
  return effect.name === undefined ? { verb: `${verb} a ${noun}`, name: null, after: tail } : { verb: `${verb} ${noun}`, name: effect.name, after: tail };
}
