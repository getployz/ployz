import "@tanstack/react-start/server-only";
import type { JoinApproval, Signed } from "@ployz/sdk";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { CLUSTER_UNREACHABLE } from "#/modules/machines/enrollment.server";
import { requireAccess } from "#/modules/organization/access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { rpcErrorMessage } from "#/modules/runtime/ployz.server";
import { Conflict, Validation } from "#/server/public-error";

/**
 * The joins waiting on the Cluster for a person to approve them, as the Server Cloud reaches drafted them. None when no
 * Server answers: the page offers what it can read.
 */
export const pendingPasskeyJoins = Effect.fn("PasskeyJoin.pending")(function* (actor: Actor, input: { readonly organizationSlug: string }) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:add");
  const session = yield* (yield* OrganizationRuntime).open(organization.id);
  const none: readonly JoinApproval[] = [];
  if (session.status !== "connected") return none;
  return yield* session.connected.pendingJoins().pipe(
    Effect.catch((error) => Effect.logWarning("Could not read the joins waiting for approval.", error).pipe(Effect.as(none))),
  );
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
