import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { resolveCaller } from "#/modules/identity/caller.server";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { member, session, user } from "#/modules/identity/tables";
import { InngestClient } from "#/modules/inngest/client";
import { serverAccessRetireRequestedEvent } from "#/modules/inngest/events";
import { removeMembership } from "#/modules/organization/membership.server";
import { organization } from "#/modules/organization/tables";
import { Auth, type AuthService } from "#/server/auth.server";
import type { Database } from "#/server/database.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ACME = "00000000-0000-4000-8000-00000000b001";
const OTHER = "00000000-0000-4000-8000-00000000b002";

const inngest = new Inngest({ id: "membership-test" });
const send = vi.spyOn(inngest, "send");
const noSession = asTestDouble<AuthService>()({ getSession: () => Effect.succeed(null) });

let harness: PostgresTestHarness;
let ada: string;
let bob: string;
let carol: string;

const run = <A, E>(effect: Effect.Effect<A, E, Database | InngestClient | Auth>) =>
  harness.runEffect(effect.pipe(Effect.provideService(InngestClient, inngest), Effect.provideService(Auth, noSession)));

const remove = (organizationId: string, userId: string) => run(removeMembership(organizationId, userId).pipe(
  Effect.as("removed"),
  Effect.catchTags({ Conflict: () => Effect.succeed("conflict"), NotFound: () => Effect.succeed("not_found") }),
));

const tokenFor = async (userId: string, organizationId: string, slug: string) => (await run(createOrganizationToken(
  { userId, organization: { id: organizationId, slug }, credential: { kind: "session", id: "session-1" }, role: null, support: null },
  { name: "laptop", expiresInDays: 1 },
))).secret;

const callerWith = (secret: string) => run(resolveCaller(new Headers({ authorization: `Bearer ${secret}` })).pipe(
  Effect.map((caller) => caller.role),
  Effect.catchTag("Unauthorized", () => Effect.succeed("unauthorized")),
));

const activeOrganization = async (userId: string) => (await harness.db.select({ id: session.activeOrganizationId, slug: session.activeOrganizationSlug })
  .from(session).where(eq(session.userId, userId)))[0];

const isMember = async (organizationId: string, userId: string) => (await harness.db.select().from(member)
  .where(and(eq(member.organizationId, organizationId), eq(member.userId, userId)))).length === 1;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
    { email: "carol@example.test", name: "Carol" },
  ]).returning({ id: user.id });
  [ada, bob, carol] = users.map((row) => row.id) as [string, string, string];
  await harness.db.insert(organization).values([
    { id: ACME, name: "Acme", slug: "acme" },
    { id: OTHER, name: "Other", slug: "other" },
  ]);
  await harness.db.insert(member).values([
    { userId: ada, organizationId: ACME, role: "owner" },
    { userId: bob, organizationId: ACME, role: "member" },
    { userId: carol, organizationId: ACME, role: "admin" },
    { userId: bob, organizationId: OTHER, role: "owner" },
  ]);
  const expiresAt = new Date(Date.now() + 86_400_000);
  await harness.db.insert(session).values([
    { userId: bob, token: "bob-session", expiresAt, activeOrganizationId: ACME, activeOrganizationSlug: "acme" },
    { userId: carol, token: "carol-session", expiresAt, activeOrganizationId: ACME, activeOrganizationSlug: "acme" },
  ]);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

beforeEach(() => {
  send.mockReset();
  send.mockResolvedValue({ ids: [] });
});

describe("removeMembership", () => {
  it("refuses to remove the last owner and changes nothing", async () => {
    expect(await remove(ACME, ada)).toBe("conflict");
    expect(await isMember(ACME, ada)).toBe(true);
    expect(send).not.toHaveBeenCalled();
  });

  it("ends a membership: its tokens stop working, its sessions move on, and its Server access is retired", async () => {
    const bobs = await tokenFor(bob, ACME, "acme");
    const adas = await tokenFor(ada, ACME, "acme");
    expect(await callerWith(bobs)).toBe("member");

    expect(await remove(ACME, bob)).toBe("removed");

    expect(await isMember(ACME, bob)).toBe(false);
    expect(await callerWith(bobs)).toBe("unauthorized");
    expect(await callerWith(adas)).toBe("owner");
    expect(await activeOrganization(bob)).toEqual({ id: OTHER, slug: "other" });
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: serverAccessRetireRequestedEvent }));
  });

  it("leaves a session with no Organization when it was the user's last one", async () => {
    expect(await remove(ACME, carol)).toBe("removed");
    expect(await activeOrganization(carol)).toEqual({ id: null, slug: null });
  });

  it("refuses someone who is not a member", async () => {
    expect(await remove(ACME, carol)).toBe("not_found");
    expect(send).not.toHaveBeenCalled();
  });
});
