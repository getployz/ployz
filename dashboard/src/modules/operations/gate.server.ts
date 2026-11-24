import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { ConfigCommand, JsonValue } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { canonicalJson } from "#/lib/json";
import { refusal, type StoreRefusal } from "#/lib/refusal";
import { Uuid } from "#/lib/schema";
import { isProtectedAsk } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { type OperationAsked, operationAsked } from "#/modules/operations/asked";
import { isOwnedOnlyByAgents } from "#/modules/sandbox/claim-required.server";
import type { Database } from "#/server/database.server";

/** The header that carries the digest a person said yes to, when Cloud asked for one. */
export const APPROVAL_HEADER = "x-ployz-approval";

/** What a protector-only write answers anyone else. */
export const OWNER_OR_ADMIN = "An owner or admin does this.";

/** What every ask for a yes ends with: an older `ployz` reads the ask but can't answer it. */
export const UPDATE_IF_UNASKED = "If ployz didn't ask yes or no, update it.";

/** The Store's ask for a yes as `ployz` reads it. Any other refusal is unchanged. */
export const askedOfCli = (refused: StoreRefusal): StoreRefusal =>
  refused.code === "approval_required"
    ? { ...refused, message: `${refused.message}${/[.!?]$/.test(refused.message) ? "" : "."} ${UPDATE_IF_UNASKED}` }
    : refused;

/**
 * The refusal for an approval ID an older `ployz` carries: Cloud keeps no approvals now, so one names nothing that could
 * run. Null for a digest, or no header.
 */
export const olderApproval = (carried: string | null): StoreRefusal | null =>
  carried !== null && Schema.is(Uuid)(carried)
    ? refusal("failed_precondition", "This approval is from an older ployz. Update ployz, then run the command again.")
    : null;

/** Whether a person could say yes in this Organization: an Agent sandbox nobody claimed has only agents. */
export const hasApprover = (organizationId: string) =>
  isOwnedOnlyByAgents(organizationId).pipe(Effect.map((agentsOnly) => !agentsOnly), Effect.orDie);

/**
 * The digest a person says yes to: the verb and what it acts on, so it covers no other command or target. Protection is
 * part of it, so a yes doesn't survive the Environment being protected or unprotected.
 */
export function operationDigest({ subject, verb, preview, effects, protected: guarded }: Pick<OperationAsked, "subject" | "verb" | "preview" | "effects" | "protected">) {
  const reviewed: JsonValue = guarded === undefined ? { subject, preview, effects } : { subject, preview, effects, protected: guarded };
  return `${verb}:${createHash("sha256").update(canonicalJson(reviewed)).digest("hex")}`;
}

const sentence = (text: string) => `${text.charAt(0).toUpperCase()}${text.slice(1)}`;

/** An agent's destructive call, refused with where a person does it instead. */
export const agentRefusal = (asked: Pick<OperationAsked, "verb" | "name" | "ployz">): StoreRefusal =>
  refusal("forbidden", `${sentence(operationAsked(asked.verb, asked.name))} needs a person. Do it in Ployz Cloud ${asked.ployz === undefined ? "(or with ployz)" : `(or run \`${asked.ployz}\`)`}.`);

/** What an agent's Store write is called when it needs a person. */
const storeWriteWord = (command: ConfigCommand) => {
  switch (command.command) {
    case "admit":
      return command.admit === "shut_down" ? "shutdown" : command.admit;
    case "publish":
      return "save";
    case "remove":
      return "removal";
    case "cancel":
    case "sync":
    case "take":
      return command.command;
    default:
      return "change";
  }
};

/**
 * An agent's Store write the Store asked a person about, refused with where a person does it: an owner or admin, in a
 * Protected Environment the agent's member can't protect. No agent says yes.
 */
export const needsPerson = (command: ConfigCommand, asked: StoreRefusal): StoreRefusal => {
  if (isProtectedAsk(asked)) return refusal("forbidden", OWNER_OR_ADMIN, { protected: true });
  return refusal("forbidden", `This ${storeWriteWord(command)} needs a person. Do it in Ployz Cloud (or with ployz).`);
};

const previewed = new Set(["set_cluster_name", "set_ingress_doors"]);

/** Cloud's ask for a yes: what `asked` destroys and the digest a resend carries once a person says yes. Nothing is kept. */
const yesRequired = (caller: Caller, asked: OperationAsked, digest: string): StoreRefusal => {
  const operation = { verb: asked.verb, name: asked.name, subject: asked.subject };
  return {
    code: "approval_required",
    message: `${sentence(operationAsked(asked.verb, asked.name))} needs your yes. Nothing ran. ${UPDATE_IF_UNASKED}`,
    details: {
      effects: asked.effects,
      approval: digest,
      operation: previewed.has(asked.verb) ? { ...operation, preview: asked.preview } : operation,
      organization: caller.organization.slug,
    },
  };
};

type Gated = { readonly ok: true } | { readonly ok: false; readonly refusal: StoreRefusal };

/** Asks a person before `asked` runs, as `gateOperation` does for one Caller and the digest it carries. */
export type Gate = (asked: OperationAsked) => Effect.Effect<Gated, never, Database>;

/**
 * Whether `asked` runs now for `caller`, who carries `approved`, the digest a person said yes to. A dashboard confirm is
 * its own yes. A protector-only ask is refused, and so is an agent's destructive one, or any it carries a yes for: only
 * a person says yes. Anyone else runs it only with the digest of exactly this plan, so a plan that moved since asks
 * again, even one that now destroys nothing.
 */
export const gateOperation = Effect.fn("Operations.gate")(function* (caller: Caller, approved: string | null, asked: OperationAsked) {
  const older = olderApproval(approved);
  if (older !== null) return { ok: false, refusal: older } satisfies Gated;
  if (asked.protected !== undefined) return { ok: false, refusal: refusal("forbidden", OWNER_OR_ADMIN, { protected: { environment: asked.protected } }) } satisfies Gated;
  if (caller.via === "dashboard") return { ok: true } satisfies Gated;
  if (approved !== null) {
    if (caller.via === "agent") return { ok: false, refusal: agentRefusal(asked) } satisfies Gated;
    const digest = operationDigest(asked);
    return approved === digest ? { ok: true } satisfies Gated : { ok: false, refusal: yesRequired(caller, asked, digest) } satisfies Gated;
  }
  if (asked.effects.length === 0 && asked.alwaysAsks === undefined) return { ok: true } satisfies Gated;
  if (!(yield* hasApprover(caller.organization.id))) return { ok: true } satisfies Gated;
  if (caller.via === "agent") return { ok: false, refusal: agentRefusal(asked) } satisfies Gated;
  return { ok: false, refusal: yesRequired(caller, asked, operationDigest(asked)) } satisfies Gated;
});
