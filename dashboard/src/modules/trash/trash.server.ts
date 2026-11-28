import "@tanstack/react-start/server-only";
import type { MachineId, Signed, TrashList } from "@ployz/sdk";
import { Effect } from "effect";
import { SERVERS_UNANSWERED_MESSAGE } from "#/lib/refusal";
import type { Actor } from "#/modules/identity/actor";
import { refusalWords } from "#/modules/machines/server-web-traffic.server";
import { requireAccess } from "#/modules/organization/access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Validation } from "#/server/public-error";

/** One Server's Trash as its page shows it: what it lists, or why it couldn't be read. */
export type ServerTrash =
  | { readonly kind: "listed"; readonly trash: TrashList }
  | { readonly kind: "unread"; readonly message: string };

const refuse = (message: string) => new Validation({ message, userFacing: true });

/** The passkey's approval as the browser sent it. Only its shape is read here: the Server checks all of it. */
const approvalOf = (text: string) =>
  // SAFETY: only the shape is assumed; the SDK and the Server parse the approval in full and refuse one that doesn't check out.
  Effect.try({ try: () => JSON.parse(text) as Signed, catch: () => refuse("That approval didn't read. Try again.") });

const connected = Effect.fn("Trash.session")(function* (organizationId: string) {
  const opened = yield* (yield* OrganizationRuntime).open(organizationId);
  return opened.status === "connected" ? opened.connected : null;
});

const withSession = <A, E, R>(organizationId: string, work: (session: PloyzSession) => Effect.Effect<A, E, R>) =>
  Effect.gen(function* () {
    const session = yield* connected(organizationId);
    if (session === null) return yield* refuse(SERVERS_UNANSWERED_MESSAGE);
    return yield* work(session);
  }).pipe(Effect.scoped);

/** `machineId`'s Trash, read from that Server when its page opens. A Server that can't say is shown as unread. */
export const listServerTrash = Effect.fn("Trash.list")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly machineId: MachineId },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:read");
  const session = yield* connected(organization.id);
  if (session === null) return { kind: "unread", message: SERVERS_UNANSWERED_MESSAGE } satisfies ServerTrash;
  return yield* session.listTrash(input.machineId).pipe(
    Effect.map((trash): ServerTrash => ({ kind: "listed", trash })),
    Effect.catch((error) => Effect.succeed<ServerTrash>({ kind: "unread", message: refusalWords(error) })),
  );
}, Effect.scoped);

/**
 * Restore the entries `approval` names on `machineId`. Cloud only carries the approval: the Server restores an entry
 * only when the person's passkey signed that entry on that Server.
 */
export const restoreServerTrash = Effect.fn("Trash.restore")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly machineId: MachineId; readonly approval: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:operate");
  const approval = yield* approvalOf(input.approval);
  return yield* withSession(organization.id, (session) => session.restoreTrash(input.machineId, approval).pipe(
    Effect.mapError((error) => refuse(refusalWords(error))),
  ));
});

/** Empty exactly the entries `approval` names on `machineId`, which the Server checks against the person's passkey. */
export const emptyServerTrash = Effect.fn("Trash.empty")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly machineId: MachineId; readonly approval: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:operate");
  const approval = yield* approvalOf(input.approval);
  return yield* withSession(organization.id, (session) => session.emptyTrash(input.machineId, approval).pipe(
    Effect.mapError((error) => refuse(refusalWords(error))),
  ));
});
