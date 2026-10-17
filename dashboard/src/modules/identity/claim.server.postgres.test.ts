import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect, Exit, Layer } from "effect";
import { expect } from "vitest";
import { organizationBillingState } from "#/modules/billing/tables";
import { gateOperation, type OperationAsked } from "#/modules/approvals/approvals.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { githubInstallation, organizationGithubInstallation } from "#/modules/github/tables";
import { signUpAnonymous } from "#/modules/identity/anonymous-signup.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { type ClaimedAction, claimUrlFor, isAnonymousOrganization, requireClaimed } from "#/modules/identity/claim-required.server";
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

/** A person whose account predates the sandbox: whatever Organization they have is theirs from before. */
const returning = Effect.fn(function* (name: string, email: string) {
  const found = yield* person(name, email);
  const { drizzle } = yield* Database;
  yield* drizzle.update(user).set({ createdAt: new Date(Date.now() - 30 * DAY_MS) }).where(eq(user.id, found.actor.userId));
  return found;
});

const organizationIds = Effect.fn(function* () {
  const { drizzle } = yield* Database;
  return (yield* drizzle.select({ id: organization.id }).from(organization)).map((row) => row.id).sort();
});

const membersOf = Effect.fn(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ userId: member.userId, role: member.role }).from(member).where(eq(member.organizationId, organizationId));
});

const bearer = (token: string) => new Headers({ authorization: `Bearer ${token}` });

/** A drain that removes a service: destructive, so a claimed Organization's agent must ask a person. */
const DRAIN: OperationAsked = {
  subject: `server:${machineId("a")}`,
  verb: "drain",
  name: "fra-1",
  preview: { server: machineId("a"), moves: ["shop.web"], stays: [], retires: ["shop.edge"] },
  effects: [{ kind: "removes_service", name: "shop.edge", node: "edge", path: "services/shop.edge" }],
};

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
    const gated = yield* gateOperation({ ...caller, via: "agent" }, null, DRAIN);
    expect(gated).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
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

it.live("a person whose account predates the sandbox keeps their Organization even when nothing ever happened in it", () =>
  inCloud(Effect.gen(function* () {
    const ada = yield* returning("Ada", "ada@example.test");
    const agent = yield* sandbox();
    yield* claimOrganization(ada.actor, agent.secret);
    expect(yield* organizationIds()).toEqual([agent.organization.id, ada.personalOrganizationId].sort());
  })));

it.live("a new person's Organization with billing state or a GitHub installation survives the claim", () =>
  inCloud(Effect.gen(function* () {
    const first = yield* sandbox();
    const second = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const bob = yield* person("Bob", "bob@example.test");
    const { drizzle } = yield* Database;
    yield* drizzle.insert(organizationBillingState).values({ organizationId: ada.personalOrganizationId });
    yield* drizzle.insert(githubInstallation).values({ installationId: 4242, accountLogin: "bob", accountType: "User" });
    yield* drizzle.insert(organizationGithubInstallation).values({
      organizationId: bob.personalOrganizationId, installationId: 4242, connectedByUserId: bob.actor.userId, connectedGithubLogin: "bob",
    });
    yield* claimOrganization(ada.actor, first.secret);
    yield* claimOrganization(bob.actor, second.secret);
    expect(yield* organizationIds()).toEqual([first.organization.id, second.organization.id, ada.personalOrganizationId, bob.personalOrganizationId].sort());
  })));

it.live("a repeat claim deletes nothing, even once the person's own Organization has emptied", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const { drizzle } = yield* Database;
    yield* drizzle.insert(fleetToken).values({
      organizationId: ada.personalOrganizationId, name: "fleet", tokenHash: "c".repeat(64), expiresAt: new Date(Date.now() + DAY_MS), createdByUserId: ada.actor.userId,
    });
    yield* claimOrganization(ada.actor, agent.secret);
    yield* drizzle.delete(fleetToken).where(eq(fleetToken.organizationId, ada.personalOrganizationId));
    yield* claimOrganization(ada.actor, agent.secret);
    expect(yield* organizationIds()).toEqual([agent.organization.id, ada.personalOrganizationId].sort());
  })));

it.live("the owner claiming again leaves their sessions in whatever Organization they moved to", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* returning("Ada", "ada@example.test");
    yield* claimOrganization(ada.actor, agent.secret);
    const { drizzle } = yield* Database;
    yield* drizzle.update(session).set({ activeOrganizationId: ada.personalOrganizationId, activeOrganizationSlug: null }).where(eq(session.userId, ada.actor.userId));
    yield* claimOrganization(ada.actor, agent.secret);
    expect(yield* drizzle.select({ activeOrganizationId: session.activeOrganizationId }).from(session).where(eq(session.userId, ada.actor.userId)))
      .toEqual([{ activeOrganizationId: ada.personalOrganizationId }]);
  })));

it.live("a person already in the sandbox keeps it as its one owner", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const { drizzle } = yield* Database;
    yield* drizzle.insert(member).values({ userId: ada.actor.userId, organizationId: agent.organization.id, role: "member" });
    expect(yield* claimOrganization(ada.actor, agent.secret)).toEqual({ slug: agent.organization.slug });
    expect(yield* membersOf(agent.organization.id)).toEqual([{ userId: ada.actor.userId, role: "owner" }]);
    expect(yield* drizzle.select().from(user).where(eq(user.id, agent.userId))).toEqual([]);
  })));

it.live("the claim URL a claim_required refusal names is the one that claims the sandbox", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const refusal = yield* Effect.flip(requireClaimed(agent.organization.id, "billing"));
    const claimUrl = yield* claimUrlFor(agent.organization.id);
    expect(refusal).toMatchObject({ details: { claim_url: claimUrl } });
    const secret = new URL(claimUrl).pathname.split("/").at(-1) ?? "";
    const ada = yield* person("Ada", "ada@example.test");
    expect(yield* claimOrganization(ada.actor, secret)).toEqual({ slug: agent.organization.slug });
    expect(yield* isAnonymousOrganization(agent.organization.id)).toBe(false);
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

it.live("the claim page reads the sandbox while it waits, answers the owner's slug after, says it was taken to anyone else, and knows no wrong secret", () =>
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
    expect(yield* readClaim(agent.secret, bob.actor)).toEqual({ state: "taken" });
    expect(yield* readClaim(agent.secret, null)).toEqual({ state: "taken" });
    expect(yield* readClaim("not-a-secret", bob.actor)).toEqual({ state: "expired" });
  })));

it.live("a second visit after the claim changes nothing: claiming again answers the same slug and leaves the sessions where they are; anyone else hears it was kept; a wrong or expired secret is refused", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const bob = yield* person("Bob", "bob@example.test");
    expect((yield* Effect.flip(claimOrganization(ada.actor, "not-a-secret")))._tag).toBe("NotFound");

    yield* claimOrganization(ada.actor, agent.secret);
    const { drizzle } = yield* Database;
    const snapshot = () => Effect.all({
      organization: drizzle.select().from(organization).where(eq(organization.id, agent.organization.id)),
      members: membersOf(agent.organization.id),
      tokens: drizzle.select().from(organizationToken).where(eq(organizationToken.organizationId, agent.organization.id)),
      users: drizzle.select({ id: user.id }).from(user).orderBy(user.id),
    });
    const claimed = yield* snapshot();
    yield* drizzle.update(session).set({ activeOrganizationId: null, activeOrganizationSlug: null }).where(eq(session.userId, ada.actor.userId));
    expect(yield* claimOrganization(ada.actor, agent.secret)).toEqual({ slug: agent.organization.slug });
    expect(yield* readClaim(agent.secret, ada.actor)).toEqual({ state: "claimed", organization: { slug: agent.organization.slug } });
    expect(yield* readClaim(agent.secret, bob.actor)).toEqual({ state: "taken" });
    expect(yield* Effect.flip(claimOrganization(bob.actor, agent.secret))).toMatchObject({ _tag: "Conflict", message: "Someone already kept this sandbox." });
    expect(yield* snapshot()).toEqual(claimed);
    expect(claimed.members).toEqual([{ userId: ada.actor.userId, role: "owner" }]);
    expect(yield* drizzle.select({ activeOrganizationId: session.activeOrganizationId }).from(session).where(eq(session.userId, ada.actor.userId)))
      .toEqual([{ activeOrganizationId: null }]);

    const lapsed = yield* sandbox();
    yield* drizzle.update(organizationToken).set({ expiresAt: new Date(Date.now() - 1000) }).where(eq(organizationToken.organizationId, lapsed.organization.id));
    expect(yield* readClaim(lapsed.secret, null)).toEqual({ state: "expired" });
    const refused = yield* Effect.flip(claimOrganization(bob.actor, lapsed.secret));
    expect(refused).toMatchObject({ _tag: "Conflict", message: "This sandbox expired." });
    expect(yield* drizzle.select().from(user).where(eq(user.id, lapsed.userId))).toHaveLength(1);
  })));

it.live("two people claiming at once: one keeps the sandbox, the other hears it was kept, and it has one owner", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const bob = yield* person("Bob", "bob@example.test");
    const exits = yield* Effect.all(
      [Effect.exit(claimOrganization(ada.actor, agent.secret)), Effect.exit(claimOrganization(bob.actor, agent.secret))],
      { concurrency: "unbounded" },
    );
    expect(exits.filter(Exit.isSuccess)).toHaveLength(1);
    expect(String(exits.find(Exit.isFailure)?.cause)).toContain("Someone already kept this sandbox.");
    const members = yield* membersOf(agent.organization.id);
    expect(members).toHaveLength(1);
    expect(members[0]?.role).toBe("owner");
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select().from(user).where(eq(user.isAnonymous, true))).toEqual([]);
  })));

it.live("claims 20 sandboxes with a p95 under 200 ms", () =>
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
    expect(p95).toBeLessThan(200);
  })));
