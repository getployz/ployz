import "@tanstack/react-start/server-only";
import { randomInt, randomUUID } from "node:crypto";
import { Effect } from "effect";
import { mintOrganizationToken } from "#/modules/identity/organization-token.server";
import { member, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { allocateUnique } from "#/utils/slug";

export const ANONYMOUS_ORGANIZATION_NAME = "Agent sandbox";
export const ANONYMOUS_TOKEN_TTL_MS = 24 * 60 * 60 * 1000;

const BASE32 = "abcdefghijklmnopqrstuvwxyz234567";
const base32Suffix = () => Array.from({ length: 8 }, () => BASE32[randomInt(BASE32.length)]).join("");

/**
 * A user with no sign-in, owning a fresh Organization, and a token that acts for it for 24 hours. The user is
 * inserted directly, never through better-auth, so its user-created hook never makes a "<name>'s Projects" org.
 */
export const signUpAnonymous = Effect.fn("Identity.signUpAnonymous")(function* () {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const suffix = base32Suffix();
    const [created] = yield* drizzle
      .insert(user)
      .values({ email: `${randomUUID()}@anonymous.ployz.invalid`, name: `Agent ${suffix}`, isAnonymous: true })
      .returning({ id: user.id });
    if (created === undefined) return yield* Effect.die("insert returned no row");
    const sandbox = yield* allocateUnique({
      tryAttempt: (attempt) =>
        Effect.gen(function* () {
          const [row] = yield* drizzle
            .insert(organization)
            .values({ name: ANONYMOUS_ORGANIZATION_NAME, slug: `anon-${attempt === 0 ? suffix : base32Suffix()}` })
            .onConflictDoNothing()
            .returning({ id: organization.id, slug: organization.slug });
          return row ?? null;
        }),
      exhausted: new Conflict({ message: "Could not allocate an anonymous organization slug." }),
    });
    yield* drizzle.insert(member).values({ userId: created.id, organizationId: sandbox.id, role: "owner" });
    const token = yield* mintOrganizationToken({
      organizationId: sandbox.id,
      userId: created.id,
      name: ANONYMOUS_ORGANIZATION_NAME,
      kind: "agent",
      expiresAt: new Date(Date.now() + ANONYMOUS_TOKEN_TTL_MS),
    });
    return { userId: created.id, organization: sandbox, token: token.secret, expiresAt: token.expiresAt };
  }));
});
