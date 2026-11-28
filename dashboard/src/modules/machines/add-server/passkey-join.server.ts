import "@tanstack/react-start/server-only";
import type { FirstKeyHint, JoinApproval, PendingJoins, Signed } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { CLUSTER_UNREACHABLE } from "#/modules/machines/enrollment.server";
import { joinCode } from "#/modules/machines/join-code.server";
import { requireAccess } from "#/modules/organization/access.server";
import { passkeyRequest } from "#/modules/passkeys/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { rpcErrorMessage } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { Conflict, Validation } from "#/server/public-error";

/** A join waiting for a person's approval, as the page shows it. */
export type PendingJoin = {
  readonly join: JoinApproval;
  /** The code `ployz device approve --join` takes. */
  readonly code: string;
  /** The key the Server trusts from its install command, when it said: only that key's approval lets it in. */
  readonly expected: FirstKeyHint | null;
  /** This person's passkey the Server waits for. Null when it waits for no key it named, or for someone else's. */
  readonly credentialId: string | null;
};

/**
 * The joins waiting on the Cluster for a person to approve them, as the Server Cloud reaches drafted them. None when no
 * Server answers: the page offers what it can read.
 */
export const pendingPasskeyJoins = Effect.fn("PasskeyJoin.pending")(function* (actor: Actor, input: { readonly organizationSlug: string }) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:add");
  const session = yield* (yield* OrganizationRuntime).open(organization.id);
  const none: readonly PendingJoin[] = [];
  if (session.status !== "connected") return none;
  const pending = yield* session.connected.pendingJoins().pipe(
    Effect.map((pending): PendingJoins | null => pending),
    Effect.catch((error) => Effect.logWarning("Could not read the joins waiting for approval.", error).pipe(Effect.as(null))),
  );
  if (pending === null || pending.joins.length === 0) return none;
  const { drizzle } = yield* Database;
  const own = new Map((yield* drizzle.select({ code: passkeyRequest.code, credentialId: passkeyRequest.credentialId })
    .from(passkeyRequest)
    .where(and(eq(passkeyRequest.organizationId, organization.id), eq(passkeyRequest.userId, actor.userId))))
    .map((row) => [row.code, row.credentialId]));
  return pending.joins.map((join): PendingJoin => {
    const expected = pending.first_keys?.[join.machine.id] ?? null;
    return {
      join,
      code: joinCode(join.machine.id, join.machine.public_key),
      expected,
      credentialId: expected?.kind === "passkey" ? own.get(expected.code) ?? null : null,
    };
  });
}, Effect.scoped);

/**
 * Hand the Server the join waits on a person's passkey-signed approval. Cloud only relays it: the Server checks the
 * passkey and the join it signed, and refuses anything else.
 */
export const approvePasskeyJoin = Effect.fn("PasskeyJoin.approve")(function* (actor: Actor, input: { readonly organizationSlug: string; readonly approval: string }) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:add");
  // SAFETY: only the shape is assumed; the Server parses the approval in full and refuses one that doesn't check out.
  const approval = yield* Effect.try({ try: () => JSON.parse(input.approval) as Signed, catch: () => new Validation({ message: "That approval didn't read. Try again.", userFacing: true }) });
  const session = yield* (yield* OrganizationRuntime).open(organization.id);
  if (session.status !== "connected") return yield* new Conflict({ message: CLUSTER_UNREACHABLE, userFacing: true });
  yield* session.connected.approveJoin(approval).pipe(Effect.mapError((error) => new Conflict({
    message: rpcErrorMessage(error) ?? "Your servers didn't take this approval. Try again.",
    userFacing: true,
  })));
}, Effect.scoped);
