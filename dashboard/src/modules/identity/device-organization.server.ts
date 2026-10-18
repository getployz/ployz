import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { deviceCode, member, verification } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";

const bindingIdentifier = (code: string) => `device-organization:${code}`;

/**
 * Records the Organization the `/device` page named, the approving session's, for the code `userId` just approved.
 * better-auth deletes the code when the CLI redeems it, so the binding lives beside it until then.
 */
export const bindDeviceOrganization = Effect.fn("Identity.bindDeviceOrganization")(
  function* (userId: string, userCode: string, organizationId: string) {
    const { drizzle } = yield* Database;
    // better-auth also accepts the default alphabet's code typed with separators or in lower case.
    const codes = [userCode, userCode.replace(/[^a-zA-Z0-9]/g, "").toUpperCase()];
    const [approved] = yield* drizzle.select({ deviceCode: deviceCode.deviceCode, expiresAt: deviceCode.expiresAt })
      .from(deviceCode)
      .where(and(eq(deviceCode.userId, userId), eq(deviceCode.status, "approved"), inArray(deviceCode.userCode, codes)))
      .limit(1);
    if (approved === undefined) return;
    yield* drizzle.insert(verification).values({
      identifier: bindingIdentifier(approved.deviceCode),
      value: organizationId,
      expiresAt: approved.expiresAt,
    });
  },
);

/** The Organization approval bound to this redeemed code, consumed, while `userId` is still a member of it. */
export const takeDeviceOrganization = Effect.fn("Identity.takeDeviceOrganization")(function* (code: string, userId: string) {
  const { drizzle } = yield* Database;
  const [bound] = yield* drizzle.delete(verification)
    .where(eq(verification.identifier, bindingIdentifier(code)))
    .returning({ organizationId: verification.value });
  if (bound === undefined) return null;
  const [membership] = yield* drizzle.select({ organizationId: member.organizationId }).from(member)
    .where(and(eq(member.userId, userId), eq(member.organizationId, bound.organizationId)))
    .limit(1);
  return membership?.organizationId ?? null;
});
