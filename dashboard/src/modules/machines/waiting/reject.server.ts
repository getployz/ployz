import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { waitingServer } from "#/modules/machines/waiting/tables";
import { requireAccess } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";

export const rejectWaitingServers = Effect.fn("WaitingServer.reject")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly ids: ReadonlyArray<string> },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const database = yield* Database;
  const selected = and(
    eq(waitingServer.organizationId, organization.id),
    inArray(waitingServer.id, [...input.ids]),
    isNull(waitingServer.rejectedAt),
  );
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // Every writer of several waiting rows locks them in id order first, so two of them can't deadlock.
    yield* drizzle.select({ id: waitingServer.id }).from(waitingServer).where(selected).orderBy(waitingServer.id).for("update");
    const rejected = yield* drizzle
      .update(waitingServer)
      .set({ rejectedAt: new Date() })
      .where(selected)
      .returning({ id: waitingServer.id });
    return { rejected: rejected.length };
  }));
});
