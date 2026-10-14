import { and, asc, eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { resolveCaller } from "#/modules/identity/caller.server";
import { createOrganizationToken } from "#/modules/identity/organization-token.server";
import { member, organizationToken, session, user } from "#/modules/identity/tables";
import { InngestClient } from "#/modules/inngest/client";
import { serverAccessRetireRequestedEvent } from "#/modules/inngest/events";
import { removeMembership, setMemberRole } from "#/modules/organization/membership.server";
import { organization } from "#/modules/organization/tables";
import { Auth, type AuthService } from "#/server/auth.server";
import type { Database } from "#/server/database.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";

const ACME = "00000000-0000-4000-8000-00000000b001";
const OTHER = "00000000-0000-4000-8000-00000000b002";
const DUO = "00000000-0000-4000-8000-00000000b003";
const HOME = "00000000-0000-4000-8000-00000000b004";
const SIDE = "00000000-0000-4000-8000-00000000b005";
const PAIR = "00000000-0000-4000-8000-00000000b006";
const RACE = "00000000-0000-4000-8000-00000000b007";

const inngest = new Inngest({ id: "membership-test" });
const send = vi.spyOn(inngest, "send");
const noSession = asTestDouble<AuthService>()({ getSession: () => Effect.succeed(null) });

let harness: PostgresTestHarness;
let ada: string;
let bob: string;
let carol: string;
let dan: string;
let dora: string;
let fay: string;
let gus: string;
let hal: string;
let ivy: string;

const run = <A, E>(effect: Effect.Effect<A, E, Database | InngestClient | Auth>) =>
  harness.runEffect(effect.pipe(Effect.provideService(InngestClient, inngest), Effect.provideService(Auth, noSession)));

const remove = (organizationId: string, userId: string, by: string) => run(removeMembership(organizationId, userId, by).pipe(
  Effect.as("removed"),
  Effect.catchTags({ Conflict: () => Effect.succeed("conflict"), NotFound: () => Effect.succeed("not_found") }),
));

const changeRole = (organizationId: string, userId: string, role: "owner" | "admin" | "member", by: string) => run(setMemberRole(organizationId, userId, role, by).pipe(
  Effect.as("changed"),
  Effect.catchTags({ Conflict: () => Effect.succeed("conflict"), NotFound: () => Effect.succeed("not_found") }),
));

const tokenFor = async (userId: string, organizationId: string, slug: string) => (await run(createOrganizationToken(
  { userId, organization: { id: organizationId, slug }, credential: { kind: "session", id: "session-1" }, role: null, support: null, via: "dashboard" },
  { name: "laptop", expiresInDays: 1 },
))).secret;

const callerWith = (secret: string) => run(resolveCaller(new Headers({ authorization: `Bearer ${secret}` })).pipe(
  Effect.map((caller) => caller.role),
  Effect.catchTag("Unauthorized", () => Effect.succeed("unauthorized")),
));

const activeOrganization = async (userId: string) => (await harness.db.select({ id: session.activeOrganizationId, slug: session.activeOrganizationSlug })
  .from(session).where(eq(session.userId, userId)))[0];

const sessionOrganization = async (token: string) => (await harness.db.select({ id: session.activeOrganizationId, slug: session.activeOrganizationSlug })
  .from(session).where(eq(session.token, token)))[0];

const tokenRows = async (organizationId: string, userId: string) => (await harness.db.select().from(organizationToken)
  .where(and(eq(organizationToken.organizationId, organizationId), eq(organizationToken.userId, userId)))).length;

const isMember = async (organizationId: string, userId: string) => (await harness.db.select().from(member)
  .where(and(eq(member.organizationId, organizationId), eq(member.userId, userId)))).length === 1;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  const users = await harness.db.insert(user).values([
    { email: "ada@example.test", name: "Ada" },
    { email: "bob@example.test", name: "Bob" },
    { email: "carol@example.test", name: "Carol" },
    { email: "dan@example.test", name: "Dan" },
    { email: "dora@example.test", name: "Dora" },
    { email: "fay@example.test", name: "Fay" },
    { email: "gus@example.test", name: "Gus" },
    { email: "hal@example.test", name: "Hal" },
    { email: "ivy@example.test", name: "Ivy" },
  ]).returning({ id: user.id });
  [ada, bob, carol, dan, dora, fay, gus, hal, ivy] = users.map((row) => row.id) as [string, string, string, string, string, string, string, string, string];
  await harness.db.insert(organization).values([
    { id: ACME, name: "Acme", slug: "acme" },
    { id: OTHER, name: "Other", slug: "other" },
    { id: DUO, name: "Duo", slug: "duo" },
    { id: HOME, name: "Home", slug: "home" },
    { id: SIDE, name: "Side", slug: "side" },
    { id: PAIR, name: "Pair", slug: "pair" },
    { id: RACE, name: "Race", slug: "race" },
  ]);
  await harness.db.insert(member).values([
    { userId: ada, organizationId: ACME, role: "owner" },
    { userId: bob, organizationId: ACME, role: "member" },
    { userId: carol, organizationId: ACME, role: "admin" },
    { userId: bob, organizationId: OTHER, role: "owner" },
    { userId: ada, organizationId: DUO, role: "owner" },
    { userId: dan, organizationId: HOME, role: "owner", createdAt: new Date("2026-01-01") },
    { userId: dan, organizationId: SIDE, role: "member", createdAt: new Date("2026-02-01") },
    { userId: dan, organizationId: DUO, role: "owner", createdAt: new Date("2026-03-01") },
    { userId: ada, organizationId: PAIR, role: "owner" },
    { userId: dora, organizationId: PAIR, role: "owner" },
    { userId: fay, organizationId: RACE, role: "owner" },
    { userId: gus, organizationId: RACE, role: "member" },
    { userId: hal, organizationId: RACE, role: "admin" },
    { userId: ivy, organizationId: RACE, role: "admin" },
  ]);
  const expiresAt = new Date(Date.now() + 86_400_000);
  await harness.db.insert(session).values([
    { userId: bob, token: "bob-session", expiresAt, activeOrganizationId: ACME, activeOrganizationSlug: "acme" },
    { userId: carol, token: "carol-session", expiresAt, activeOrganizationId: ACME, activeOrganizationSlug: "acme" },
    { userId: dan, token: "dan-duo", expiresAt, activeOrganizationId: DUO, activeOrganizationSlug: "duo" },
    { userId: dan, token: "dan-side", expiresAt, activeOrganizationId: SIDE, activeOrganizationSlug: "side" },
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
    expect(await remove(ACME, ada, ada)).toBe("conflict");
    expect(await isMember(ACME, ada)).toBe(true);
    expect(send).not.toHaveBeenCalled();
  });

  it("ends a membership: its tokens stop working, its sessions move on, and its Server access is retired", async () => {
    const bobs = await tokenFor(bob, ACME, "acme");
    const adas = await tokenFor(ada, ACME, "acme");
    expect(await callerWith(bobs)).toBe("member");

    expect(await remove(ACME, bob, ada)).toBe("removed");

    expect(await isMember(ACME, bob)).toBe(false);
    expect(await callerWith(bobs)).toBe("unauthorized");
    expect(await callerWith(adas)).toBe("owner");
    expect(await activeOrganization(bob)).toEqual({ id: OTHER, slug: "other" });
    expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: serverAccessRetireRequestedEvent }));
    expect(await tokenRows(ACME, bob)).toBe(0);

    await harness.db.insert(member).values({ userId: bob, organizationId: ACME, role: "member" });
    expect(await callerWith(bobs)).toBe("unauthorized");
  });

  it("removes one of two owners and leaves their sessions in other Organizations alone", async () => {
    expect(await remove(DUO, dan, ada)).toBe("removed");

    expect(await isMember(DUO, dan)).toBe(false);
    expect(await isMember(DUO, ada)).toBe(true);
    expect(await sessionOrganization("dan-duo")).toEqual({ id: HOME, slug: "home" });
    expect(await sessionOrganization("dan-side")).toEqual({ id: SIDE, slug: "side" });
  });

  it("leaves a session with no Organization when it was the user's last one", async () => {
    expect(await remove(ACME, carol, ada)).toBe("removed");
    expect(await activeOrganization(carol)).toEqual({ id: null, slug: null });
  });

  it("refuses someone who is not a member", async () => {
    expect(await remove(OTHER, carol, bob)).toBe("not_found");
    expect(send).not.toHaveBeenCalled();
  });
});

type Tx = Parameters<Parameters<typeof harness.db.transaction>[0]>[0];

const whileLocked = async <A>(organizationId: string, change: (tx: Tx) => Promise<void>, during: () => Promise<A>) => {
  const { outcome } = await harness.db.transaction(async (tx) => {
    await tx.select({ userId: member.userId }).from(member).where(eq(member.organizationId, organizationId)).orderBy(asc(member.userId)).for("update");
    await change(tx);
    const outcome = during();
    await new Promise((resolve) => setTimeout(resolve, 300));
    return { outcome };
  });
  return outcome;
};

const roleIn = async (organizationId: string, userId: string) => (await harness.db.select({ role: member.role }).from(member)
  .where(and(eq(member.organizationId, organizationId), eq(member.userId, userId))))[0]?.role ?? null;

const setRole = (organizationId: string, userId: string, role: "owner" | "admin" | "member") =>
  async (tx: Tx) => {
    await tx.update(member).set({ role }).where(and(eq(member.organizationId, organizationId), eq(member.userId, userId)));
  };

describe("a role that changes while a request waits for the lock", () => {
  it("refuses an admin who removes a member made an owner meanwhile", async () => {
    expect(await whileLocked(RACE, setRole(RACE, gus, "owner"), () => remove(RACE, gus, hal))).toBe("conflict");
    expect(await roleIn(RACE, gus)).toBe("owner");
    await harness.db.update(member).set({ role: "member" }).where(and(eq(member.organizationId, RACE), eq(member.userId, gus)));
  });

  it("refuses an admin demoted meanwhile who removes a member", async () => {
    expect(await whileLocked(RACE, setRole(RACE, hal, "member"), () => remove(RACE, gus, hal))).toBe("conflict");
    expect(await roleIn(RACE, gus)).toBe("member");
  });

  it("refuses an owner demoted meanwhile who makes another owner", async () => {
    expect(await whileLocked(RACE, setRole(RACE, fay, "admin"), () => changeRole(RACE, ivy, "owner", fay))).toBe("conflict");
    expect(await roleIn(RACE, ivy)).toBe("admin");
  });
});

describe("setMemberRole", () => {
  it("keeps an owner when one owner is demoted while the other is removed", async () => {
    const outcomes = await whileLocked(PAIR, async () => undefined, () => Promise.all([changeRole(PAIR, ada, "member", dora), remove(PAIR, dora, ada)]));
    expect(outcomes.filter((outcome) => outcome === "conflict")).toHaveLength(1);
    const owners = await harness.db.select().from(member).where(and(eq(member.organizationId, PAIR), eq(member.role, "owner")));
    expect(owners).toHaveLength(1);
  });
});
