import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { Forbidden } from "#/server/public-error";

/** A member who may operate Servers. Support reads this section but never switches the Cluster's web traffic or holds its token. */
export const requireOperator = Effect.fn("IngressDoors.requireOperator")(function* (actor: Actor, organizationSlug: string) {
  const access = yield* requireAccess(actor, organizationSlug, "servers:operate");
  if (access.kind === "support") {
    return yield* new Forbidden({ userFacing: true, message: "Only an owner or admin of this Organization can do this." });
  }
  return access;
});
