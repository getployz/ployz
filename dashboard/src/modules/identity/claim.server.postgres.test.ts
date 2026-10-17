import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect, Exit, Layer } from "effect";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { signUpAnonymous } from "#/modules/identity/anonymous-signup.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { type ClaimedAction, isAnonymousOrganization, requireClaimed } from "#/modules/identity/claim-required.server";
import { claimOrganization, readClaim } from "#/modules/identity/claim.server";
import { member, organizationToken, session, user } from "#/modules/identity/tables";
import { machineEnrollmentToken, machineRemoveAttempt } from "#/modules/machines/tables";
import { fleetToken } from "#/modules/machines/waiting/tables";
import { ensurePersonalOrganizationForUser } from "#/modules/organization/organization-state.server";
import { organization } from "#/modules/organization/tables";
import { AuthLive } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { machineId } from "#/test/ids";
import { storeTestCloud } from "#/test/store-cloud";

const DAY_MS = 24 * 60 * 60 * 1000;
const ACTIONS: ReadonlyArray<ClaimedAction> = ["add_domain", "mint_token", "invite", "billing", "github_connect"];

const inCloud = <A, E, R>(body: Effect.Effect<A, E, R>) => Effect.gen(function* () {
  const cloud = yield* storeTestCloud();
  return yield* body.pipe(Effect.provide(Layer.mergeAll(AuthLive.pipe(Layer.provide(cloud)), cloud)));
});

/** An agent signed up: its sandbox, its token, and the secret in its claim URL. */
const sandbox = Effect.fn(function* () {
  const signup = yield* signUpAnonymous();
  const secret = new URL(signup.claimUrl).pathname.split("/").at(-1) ?? "";
  return { ...signup, secret };
});

/** A person who just signed in with GitHub: a user, the Organization sign-in made for them, and a session in it. */
const person = Effect.fn(function* (name: string, email: string) {
  const { drizzle } = yield* Database;
  const id = crypto.randomUUID();
  yield* drizzle.insert(user).values({ id, email, name });
  const personalOrganizationId = yield* ensurePersonalOrganizationForUser({ id, name, email });
  const [slugRow] = yield* drizzle.select({ slug: organization.slug }).from(organization).where(eq(organization.id, personalOrganizationId));
  yield* drizzle.insert(session).values({
    userId: id, token: `session-${id}`, expiresAt: new Date(Date.now() + DAY_MS),
    activeOrganizationId: personalOrganizationId, activeOrganizationSlug: slugRow?.slug ?? null,
  });
  return { actor: { userId: id }, personalOrganizationId };
});

const membersOf = Effect.fn(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ userId: member.userId, role: member.role }).from(member).where(eq(member.organizationId, organizationId));
});

const bearer = (token: string) => new Headers({ authorization: `Bearer ${token}` });

it.live("a person who claims the sandbox becomes its sole owner and the agent's token keeps working for 90 days", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const before = Date.now();

    const claimed = yield* claimOrganization(ada.actor, agent.secret);
    expect(claimed).toEqual({ slug: agent.organization.slug });

    expect(yield* membersOf(agent.organization.id)).toEqual([{ userId: ada.actor.userId, role: "owner" }]);
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select().from(user).where(eq(user.id, agent.userId))).toEqual([]);
    const [org] = yield* drizzle.select({ claimSecretHash: organization.claimSecretHash }).from(organization).where(eq(organization.id, agent.organization.id));
    expect(org?.claimSecretHash).toBeNull();

    const caller = yield* resolveCaller(bearer(agent.token));
    expect(caller.userId).toBe(ada.actor.userId);
    expect(caller.organization).toEqual(agent.organization);
    expect(caller.role).toBe("owner");
    const [token] = yield* drizzle.select({ expiresAt: organizationToken.expiresAt }).from(organizationToken)
      .where(eq(organizationToken.organizationId, agent.organization.id));
    expect(token?.expiresAt.getTime()).toBeGreaterThanOrEqual(before + 90 * DAY_MS);
    expect(token?.expiresAt.getTime()).toBeLessThanOrEqual(Date.now() + 90 * DAY_MS);

    expect(yield* isAnonymousOrganization(agent.organization.id)).toBe(false);
    for (const action of ACTIONS) yield* requireClaimed(agent.organization.id, action);
  })));

it.live("the claim removes the empty Organization sign-in made and moves the person's sessions onto the sandbox", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    yield* claimOrganization(ada.actor, agent.secret);
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select({ id: organization.id }).from(organization)).toEqual([{ id: agent.organization.id }]);
    const sessions = yield* drizzle.select({ activeOrganizationId: session.activeOrganizationId, activeOrganizationSlug: session.activeOrganizationSlug })
      .from(session).where(eq(session.userId, ada.actor.userId));
    expect(sessions).toEqual([{ activeOrganizationId: agent.organization.id, activeOrganizationSlug: agent.organization.slug }]);
  })));

it.live("a returning person keeps an Organization they used", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const store = yield* cloudStore;
    yield* Effect.promise(() => store.write(ada.personalOrganizationId, {
      command: "create_project", id: "00000000-0000-4000-8000-00000000c602", name: "shop", default_environment: "00000000-0000-4000-8000-00000000c603",
    }));
    yield* claimOrganization(ada.actor, agent.secret);
    const { drizzle } = yield* Database;
    const ids = (yield* drizzle.select({ id: organization.id }).from(organization)).map((row) => row.id).sort();
    expect(ids).toEqual([agent.organization.id, ada.personalOrganizationId].sort());
    expect(yield* membersOf(ada.personalOrganizationId)).toEqual([{ userId: ada.actor.userId, role: "owner" }]);
  })));

it.live("every row the agent made now names the person, so deleting the anonymous user never trips a foreign key", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const { drizzle } = yield* Database;
    const expiresAt = new Date(Date.now() + DAY_MS);
    yield* drizzle.insert(fleetToken).values({ organizationId: agent.organization.id, name: "fleet", tokenHash: "a".repeat(64), expiresAt, createdByUserId: agent.userId });
    yield* drizzle.insert(machineEnrollmentToken).values({ organizationId: agent.organization.id, tokenHash: "b".repeat(64), expiresAt, createdByUserId: agent.userId });
    yield* drizzle.insert(machineRemoveAttempt).values({
      organizationId: agent.organization.id, requestedByUserId: agent.userId, machineId: machineId("0"), confirmDataLoss: [],
    });

    yield* claimOrganization(ada.actor, agent.secret);

    expect(yield* drizzle.select({ by: fleetToken.createdByUserId }).from(fleetToken)).toEqual([{ by: ada.actor.userId }]);
    expect(yield* drizzle.select({ by: machineEnrollmentToken.createdByUserId }).from(machineEnrollmentToken)).toEqual([{ by: ada.actor.userId }]);
    expect(yield* drizzle.select({ by: machineRemoveAttempt.requestedByUserId }).from(machineRemoveAttempt)).toEqual([{ by: ada.actor.userId }]);
  })));

it.live("the claim page reads the sandbox while it waits, answers the owner's slug after, and nothing to anyone else", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const store = yield* cloudStore;
    yield* Effect.promise(() => store.write(agent.organization.id, {
      command: "create_project", id: "00000000-0000-4000-8000-00000000c612", name: "shop", default_environment: "00000000-0000-4000-8000-00000000c613",
    }));
    expect(yield* readClaim(agent.secret, null)).toEqual({
      state: "pending", organization: { name: "Agent sandbox", slug: agent.organization.slug }, servers: 0, projects: 1, expiresAt: agent.expiresAt,
    });
    expect(yield* readClaim("not-a-secret", null)).toEqual({ state: "expired" });

    const ada = yield* person("Ada", "ada@example.test");
    const bob = yield* person("Bob", "bob@example.test");
    yield* claimOrganization(ada.actor, agent.secret);
    expect(yield* readClaim(agent.secret, ada.actor)).toEqual({ state: "claimed", organization: { slug: agent.organization.slug } });
    expect(yield* readClaim(agent.secret, bob.actor)).toEqual({ state: "expired" });
    expect(yield* readClaim(agent.secret, null)).toEqual({ state: "expired" });
  })));

it.live("claiming again answers the same slug; a wrong, spent or expired secret is refused", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const bob = yield* person("Bob", "bob@example.test");
    expect((yield* Effect.flip(claimOrganization(ada.actor, "not-a-secret")))._tag).toBe("NotFound");

    yield* claimOrganization(ada.actor, agent.secret);
    expect(yield* claimOrganization(ada.actor, agent.secret)).toEqual({ slug: agent.organization.slug });
    expect(yield* membersOf(agent.organization.id)).toEqual([{ userId: ada.actor.userId, role: "owner" }]);
    expect((yield* Effect.flip(claimOrganization(bob.actor, agent.secret)))._tag).toBe("NotFound");

    const lapsed = yield* sandbox();
    const { drizzle } = yield* Database;
    yield* drizzle.update(organizationToken).set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(organizationToken.organizationId, lapsed.organization.id));
    expect(yield* readClaim(lapsed.secret, null)).toEqual({ state: "expired" });
    const refused = yield* Effect.flip(claimOrganization(bob.actor, lapsed.secret));
    expect(refused).toMatchObject({ _tag: "Conflict", message: "This sandbox expired." });
    expect(yield* drizzle.select().from(user).where(eq(user.id, lapsed.userId))).toHaveLength(1);
  })));

it.live("two people claiming at once: one keeps the sandbox, the other is refused, and it has one owner", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const bob = yield* person("Bob", "bob@example.test");
    const exits = yield* Effect.all(
      [Effect.exit(claimOrganization(ada.actor, agent.secret)), Effect.exit(claimOrganization(bob.actor, agent.secret))],
      { concurrency: "unbounded" },
    );
    expect(exits.filter(Exit.isSuccess)).toHaveLength(1);
    expect(String(exits.find(Exit.isFailure)?.cause)).toContain("NotFound");
    const members = yield* membersOf(agent.organization.id);
    expect(members).toHaveLength(1);
    expect(members[0]?.role).toBe("owner");
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select().from(user).where(eq(user.isAnonymous, true))).toEqual([]);
  })));

it.live("claims 20 sandboxes with a p95 well under a second", () =>
  inCloud(Effect.gen(function* () {
    const timings: number[] = [];
    for (let i = 0; i < 20; i++) {
      const agent = yield* sandbox();
      const claimer = yield* person(`Person ${i}`, `person-${i}@example.test`);
      const started = performance.now();
      yield* claimOrganization(claimer.actor, agent.secret);
      timings.push(performance.now() - started);
    }
    const sorted = [...timings].sort((a, b) => a - b);
    const p95 = sorted[Math.ceil(sorted.length * 0.95) - 1] ?? 0;
    console.log(`claim timings ms: p50=${sorted[9]?.toFixed(1)} p95=${p95.toFixed(1)} max=${sorted.at(-1)?.toFixed(1)}`);
    expect(p95).toBeLessThan(1000);
  })));
