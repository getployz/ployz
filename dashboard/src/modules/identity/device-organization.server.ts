import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { deviceCode, verification } from "#/modules/identity/tables";
import { isMember } from "#/modules/organization/membership.server";
import { Database } from "#/server/database.server";

const bindingIdentifier = (code: string) => `device-organization:${code}`;

/**
 * Records the Organization the `/device` page named for the pending code `userId` claimed, before better-auth approves
 * it, so the CLI can never redeem an approved code without its binding. False when `userId` is not a member.
 * better-auth deletes the code when the CLI redeems it, so the binding lives beside it until then.
 */
export const bindDeviceOrganization = Effect.fn("Identity.bindDeviceOrganization")(
  function* (userId: string, userCode: string, organizationId: string) {
    if (!(yield* isMember(userId, organizationId))) return false;
    // better-auth also accepts the default alphabet's code typed with separators or in lower case.
    const codes = [userCode, userCode.replace(/[^a-zA-Z0-9]/g, "").toUpperCase()];
    const database = yield* Database;
    // The lock waits out an approval in flight, so a duplicate approve finds the code approved and leaves its binding.
    yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [pending] = yield* drizzle.select({ deviceCode: deviceCode.deviceCode, expiresAt: deviceCode.expiresAt })
        .from(deviceCode)
        .where(and(eq(deviceCode.userId, userId), eq(deviceCode.status, "pending"), inArray(deviceCode.userCode, codes)))
        .limit(1)
        .for("update");
      if (pending === undefined) return;
      const identifier = bindingIdentifier(pending.deviceCode);
      yield* drizzle.delete(verification).where(eq(verification.identifier, identifier));
      yield* drizzle.insert(verification).values({ identifier, value: organizationId, expiresAt: pending.expiresAt });
    }));
    return true;
  },
);

/** The Organization approval bound to this redeemed code, consumed, while `userId` is still a member of it. */
export const takeDeviceOrganization = Effect.fn("Identity.takeDeviceOrganization")(function* (code: string, userId: string) {
  const { drizzle } = yield* Database;
  const [bound] = yield* drizzle.delete(verification)
    .where(eq(verification.identifier, bindingIdentifier(code)))
    .returning({ organizationId: verification.value });
  if (bound === undefined) return null;
  return (yield* isMember(userId, bound.organizationId)) ? bound.organizationId : null;
});
