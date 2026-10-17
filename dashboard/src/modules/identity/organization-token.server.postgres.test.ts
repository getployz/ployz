import { Effect } from "effect";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { Caller } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { createOrganizationToken, listCredentials, mintOrganizationToken, revokeCredential } from "#/modules/identity/organization-token.server";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Auth, type AuthService } from "#/server/auth.server";
import type { Database } from "#/server/database.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ACME = "00000000-0000-4000-8000-00000000d001";

let harness: PostgresTestHarness;
const ids = { ada: "", bob: "", eve: "" };
const roles = { ada: "owner", bob: "admin", eve: "member" } satisfies Record<keyof typeof ids, Caller["role"]>;

const run = <A, E>(effect: Effect.Effect<A, E, Database>) => harness.runEffect(effect);

const callerOf = (who: keyof typeof ids): Caller => ({
  userId: ids[who],
  organization: { id: ACME, slug: "acme" },
  credential: { kind: "session", id: `${who}-session` },
  role: roles[who],
  support: null,
  via: "dashboard",
});

const tokenOf = async (who: keyof typeof ids) => (await run(createOrganizationToken(callerOf(who), { name: `${who}-ci`, expiresInDays: 1 }))).id;

const listedBy = async (who: keyof typeof ids) => (await run(listCredentials(callerOf(who)))).tokens.map((token) => token.name).sort();

const revokedBy = (who: keyof typeof ids, id: string) => run(revokeCredential(callerOf(who), id).pipe(
  Effect.as("revoked"),
  Effect.catchTag("NotFound", () => Effect.succeed("not_found")),
));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
    { email: "eve@example.test", name: "Eve" },
  ]).returning({ id: user.id });
  [ids.ada, ids.bob, ids.eve] = users.map((row) => row.id) as [string, string, string];
  await harness.db.insert(organization).values({ id: ACME, name: "Acme", slug: "acme" });
  await harness.db.insert(member).values([
    { userId: ids.ada, organizationId: ACME, role: "owner" },
    { userId: ids.bob, organizationId: ACME, role: "admin" },
    { userId: ids.eve, organizationId: ACME, role: "member" },
  ]);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

describe("organization tokens", () => {
  beforeEach(async () => {
    await harness.db.delete(organizationToken);
  });

  it("shows a member only their own tokens and lets them revoke only those", async () => {
    const ada = await tokenOf("ada");
    const eve = await tokenOf("eve");
    expect(await listedBy("eve")).toEqual(["eve-ci"]);
    expect(await revokedBy("eve", ada)).toBe("not_found");
    expect(await listedBy("ada")).toContain("ada-ci");
    expect(await revokedBy("eve", eve)).toBe("revoked");
    expect(await listedBy("eve")).toEqual([]);
  });

  it("shows owners and admins every token and lets them revoke any", async () => {
    const ada = await tokenOf("ada");
    await tokenOf("eve");
    expect(await listedBy("bob")).toEqual(["ada-ci", "eve-ci"]);
    expect(await listedBy("ada")).toEqual(["ada-ci", "eve-ci"]);
    expect(await revokedBy("bob", ada)).toBe("revoked");
    expect(await listedBy("ada")).toEqual(["eve-ci"]);
  });

  it("shows support none of the Organization's tokens and lets it revoke none", async () => {
    const ada = await tokenOf("ada");
    const support: Caller = {
      ...callerOf("bob"), userId: "00000000-0000-4000-8000-00000000d0ff", role: null,
      support: { variables: true, configs: true, logs: true },
    };
    expect((await run(listCredentials(support))).tokens).toEqual([]);
    expect(await run(revokeCredential(support, ada).pipe(
      Effect.as("revoked"),
      Effect.catchTag("NotFound", () => Effect.succeed("not_found")),
    ))).toBe("not_found");
  });

  it("acts as its maker until the expiry it was minted with, then reads as no credential", async () => {
    const noSession = asTestDouble<AuthService>()({ getSession: () => Effect.succeed(null) });
    const presented = async (expiresAt: Date) => {
      const { secret } = await run(mintOrganizationToken({ organizationId: ACME, userId: ids.eve, name: "agent", kind: "agent", expiresAt }));
      return run(resolveCaller(new Headers({ authorization: `Bearer ${secret}` })).pipe(
        Effect.map((caller) => caller.userId),
        Effect.catchTag("Unauthorized", () => Effect.succeed("unauthorized")),
        Effect.provideService(Auth, noSession),
      ));
    };
    expect(await presented(new Date(Date.now() + 60_000))).toBe(ids.eve);
    expect(await presented(new Date(Date.now() - 1_000))).toBe("unauthorized");
  });
});
