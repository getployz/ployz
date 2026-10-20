import { it } from "@effect/vitest";
import { eq, sql } from "drizzle-orm";
import { Effect, Exit, Layer, Redacted } from "effect";
import { Client } from "pg";
import { expect, vi } from "vitest";
import { organizationBillingState } from "#/modules/billing/tables";
import { gateOperation, type OperationAsked } from "#/modules/approvals/approvals.server";
import { readStore } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { githubInstallation, organizationGithubInstallation } from "#/modules/github/tables";
import { signUpAnonymous } from "#/modules/identity/anonymous-signup.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { type ClaimedAction, claimUrlFor, hashClaimSecret, isAnonymousOrganization, requireClaimed } from "#/modules/identity/claim-required.server";
import { bindClaimSignInOrganization } from "#/modules/identity/claim-sign-in.server";
import { claimOrganization, finishClaimSignIn, readClaim } from "#/modules/identity/claim.server";
import { member, organizationToken, session, user } from "#/modules/identity/tables";
import { machineEnrollmentToken, machineRemoveAttempt, organizationMachine, serverAccess } from "#/modules/machines/tables";
import { fleetToken } from "#/modules/machines/waiting/tables";
import { ensurePersonalOrganizationForUser } from "#/modules/organization/organization-state.server";
import { organization } from "#/modules/organization/tables";
import { AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
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
  const [signedIn] = yield* drizzle.insert(session).values({
    userId: id, token: `session-${id}`, expiresAt: new Date(Date.now() + DAY_MS),
    activeOrganizationId: personalOrganizationId, activeOrganizationSlug: slugRow?.slug ?? null,
  }).returning({ id: session.id });
  const actor = { userId: id, credential: { kind: "session", id: signedIn?.id ?? "" } } as const;
  return { actor, personalOrganizationId };
});

/**
 * A person whose GitHub sign-up the claim page for this secret started: the auth hook binds the Organization that
 * sign-up made to the claim, through the same writer.
 */
const claimSignUp = Effect.fn(function* (name: string, email: string, secret: string) {
  const found = yield* person(name, email);
  yield* bindClaimSignInOrganization(found.actor.userId, found.personalOrganizationId, hashClaimSecret(secret));
  return found;
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

it.live("the claim removes the empty Organization sign-in made and moves the person's session onto the sandbox", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* claimSignUp("Ada", "ada@example.test", agent.secret);
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
    const ada = yield* claimSignUp("Ada", "ada@example.test", agent.secret);
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

it.live("a person whose account predates the claim's sign-in keeps their Organization even when nothing ever happened in it", () =>
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
    const ada = yield* claimSignUp("Ada", "ada@example.test", first.secret);
    const bob = yield* claimSignUp("Bob", "bob@example.test", second.secret);
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
    const ada = yield* claimSignUp("Ada", "ada@example.test", agent.secret);
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
    const encryption = yield* SecretEncryption;
    yield* drizzle.insert(organizationMachine).values({
      organizationId: agent.organization.id, machineId: machineId("0"), clusterKey: "e".repeat(64),
      encryptedCapability: encryption.encrypt("ployz1:cloud:0"), isDialEntry: false,
    });
    yield* drizzle.insert(serverAccess).values({
      organizationId: agent.organization.id, machineId: machineId("0"), credentialId: crypto.randomUUID(), credentialKind: "token", userId: agent.userId, revokedAt: new Date(),
    });
    const [fleet] = yield* drizzle.select({ id: fleetToken.id }).from(fleetToken);
    yield* drizzle.execute(sql`
      with waiting as (
        insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip)
        values (${agent.organization.id}, ${fleet?.id}, 'key', '{}', '192.0.2.1') returning id
      )
      insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state, requested_by_user_id)
      select ${agent.organization.id}, waiting.id, ${fleet?.id}, 'founder', '{}', ${crypto.randomUUID()}, 'requested', ${agent.userId} from waiting`);

    yield* claimOrganization(ada.actor, agent.secret);

    expect(yield* drizzle.select({ by: fleetToken.createdByUserId }).from(fleetToken)).toEqual([{ by: ada.actor.userId }]);
    expect(yield* drizzle.select({ by: machineEnrollmentToken.createdByUserId }).from(machineEnrollmentToken)).toEqual([{ by: ada.actor.userId }]);
    expect(yield* drizzle.select({ by: machineRemoveAttempt.requestedByUserId }).from(machineRemoveAttempt)).toEqual([{ by: ada.actor.userId }]);
    expect(yield* drizzle.select({ by: serverAccess.userId }).from(serverAccess)).toEqual([{ by: ada.actor.userId }]);
    expect(yield* drizzle.execute<{ by: string }>(sql`select requested_by_user_id as by from server_enrollment`, "objects")).toEqual([{ by: ada.actor.userId }]);
  })));

it.live("a claim leaves alone an Organization the person made outside the sign-in the claim page started", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    yield* claimOrganization(ada.actor, agent.secret);
    const bob = yield* person("Bob", "bob@example.test");
    const later = yield* sandbox();
    yield* claimOrganization(bob.actor, later.secret);
    expect(yield* organizationIds()).toEqual([agent.organization.id, later.organization.id, ada.personalOrganizationId, bob.personalOrganizationId].sort());
  })));

it.live("an Organization with a row in any table that names an Organization survives the claim", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* claimSignUp("Ada", "ada@example.test", agent.secret);
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`create table claim_probe (organization_id uuid not null)`);
    yield* drizzle.execute(sql`insert into claim_probe values (${ada.personalOrganizationId})`);
    yield* claimOrganization(ada.actor, agent.secret);
    expect(yield* organizationIds()).toEqual([agent.organization.id, ada.personalOrganizationId].sort());
  })));

it.live("back from the GitHub sign-in the claim page started, the person owns the sandbox with no second click", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* claimSignUp("Ada", "ada@example.test", agent.secret);
    expect(yield* finishClaimSignIn(agent.secret, ada.actor)).toEqual({ state: "claimed", organization: { slug: agent.organization.slug } });
    expect(yield* membersOf(agent.organization.id)).toEqual([{ userId: ada.actor.userId, role: "owner" }]);
    expect(yield* organizationIds()).toEqual([agent.organization.id]);
  })));

it.live("a GitHub sign-up in another tab while the claim page's sign-in is out keeps its Organization, and the claim's own still goes", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    // Tab A: the claim page's GitHub sign-in signs Ada up, and the hook binds her new Organization to this claim.
    const ada = yield* claimSignUp("Ada", "ada@example.test", agent.secret);
    // Tab B: an ordinary GitHub sign-in signs Bob up; the browser's one session is now his.
    const bob = yield* person("Bob", "bob@example.test");
    // Tab A finishes as Bob: Bob gets the sandbox, and his Organization is not the claim's to remove.
    expect(yield* finishClaimSignIn(agent.secret, bob.actor)).toEqual({ state: "claimed", organization: { slug: agent.organization.slug } });
    expect(yield* organizationIds()).toEqual([agent.organization.id, ada.personalOrganizationId, bob.personalOrganizationId].sort());

    // The claim's own sign-up still loses the Organization it made when it is the one that claims.
    const later = yield* sandbox();
    const cy = yield* claimSignUp("Cy", "cy@example.test", later.secret);
    yield* finishClaimSignIn(later.secret, cy.actor);
    expect(yield* organizationIds()).not.toContain(cy.personalOrganizationId);
  })));

it.live("a binding to another claim's secret removes nothing", () =>
  inCloud(Effect.gen(function* () {
    const elsewhere = yield* sandbox();
    const agent = yield* sandbox();
    const ada = yield* claimSignUp("Ada", "ada@example.test", elsewhere.secret);
    yield* claimOrganization(ada.actor, agent.secret);
    expect(yield* organizationIds()).toContain(ada.personalOrganizationId);
  })));

it.live("an anonymous token minted while the claim runs moves to the person instead of blocking or vanishing", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const ada = yield* person("Ada", "ada@example.test");
    const database = yield* Database;
    const minted = database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.insert(organizationToken).values({
        organizationId: agent.organization.id, userId: agent.userId, name: "late", secretHash: "d".repeat(64), expiresAt: new Date(Date.now() + DAY_MS),
      });
      yield* Effect.sleep("300 millis");
    }));
    const claimed = Effect.sleep("100 millis").pipe(Effect.andThen(claimOrganization(ada.actor, agent.secret)));
    const [mint, claim] = yield* Effect.all([Effect.exit(minted), Effect.exit(claimed)], { concurrency: "unbounded" });
    expect(Exit.isSuccess(mint)).toBe(true);
    expect(Exit.isSuccess(claim)).toBe(true);
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select({ userId: organizationToken.userId }).from(organizationToken).where(eq(organizationToken.name, "late")))
      .toEqual([{ userId: ada.actor.userId }]);
  })));

it.live("rotating the encryption secret retires every outstanding claim URL, and the next refusal names one that claims", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const config = yield* AppConfig;
    const rotated = Effect.provideService(AppConfig, { ...config, encryptionSecret: Redacted.make("rotated-encryption-secret-0123456789") });
    expect(yield* readClaim(agent.secret, null).pipe(rotated)).toEqual({ state: "expired" });
    const ada = yield* person("Ada", "ada@example.test");
    expect((yield* Effect.flip(claimOrganization(ada.actor, agent.secret).pipe(rotated)))._tag).toBe("NotFound");
    const refusal = yield* Effect.flip(requireClaimed(agent.organization.id, "billing").pipe(rotated));
    const claimUrl = String((refusal as { details?: { claim_url?: string } }).details?.claim_url);
    const secret = new URL(claimUrl).pathname.split("/").at(-1) ?? "";
    expect(secret).not.toBe(agent.secret);
    expect(yield* readClaim(secret, null).pipe(rotated)).toMatchObject({ state: "pending" });
    expect(yield* claimOrganization(ada.actor, secret).pipe(rotated)).toEqual({ slug: agent.organization.slug });
  })));

it.live("the claim page reads the sandbox while it waits, answers the owner's slug after, says it was taken to anyone else, and finds nothing under a wrong or tampered secret", () =>
  inCloud(Effect.gen(function* () {
    const agent = yield* sandbox();
    const store = yield* cloudStore;
    yield* Effect.promise(() => store.write(agent.organization.id, {
      command: "create_project", id: "00000000-0000-4000-8000-00000000c612", name: "shop", default_environment: "00000000-0000-4000-8000-00000000c613",
    }));
    expect(yield* readClaim(agent.secret, null)).toEqual({
      state: "pending", organization: { name: "Agent sandbox", slug: agent.organization.slug }, servers: 0, projects: 1, hoursLeft: 24,
    });
    expect(yield* readClaim("not-a-secret", null)).toEqual({ state: "not_found" });
    const tampered = `${agent.secret.slice(0, -1)}${agent.secret.endsWith("A") ? "B" : "A"}`;
    expect(yield* readClaim(tampered, null)).toEqual({ state: "not_found" });

    const ada = yield* person("Ada", "ada@example.test");
    const bob = yield* person("Bob", "bob@example.test");
    yield* claimOrganization(ada.actor, agent.secret);
    expect(yield* readClaim(agent.secret, ada.actor)).toEqual({ state: "claimed", organization: { slug: agent.organization.slug } });
    expect(yield* readClaim(agent.secret, bob.actor)).toEqual({ state: "taken" });
    expect(yield* readClaim(agent.secret, null)).toEqual({ state: "taken" });
    expect(yield* readClaim("not-a-secret", bob.actor)).toEqual({ state: "not_found" });
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

/** Every statement `program` sends to PostgreSQL through any pg client, transaction control included: one round trip each. */
const statementsOf = <A, E, R>(program: Effect.Effect<A, E, R>) => Effect.gen(function* () {
  const query = vi.spyOn(Client.prototype, "query");
  const sent = yield* program.pipe(
    Effect.map(() => query.mock.calls.map(([statement]) => statement)),
    Effect.ensuring(Effect.sync(() => query.mockRestore())),
  );
  return sent;
});

it.live("a claim back from the claim page's sign-in takes at most 21 round trips, however many tables name an Organization", () =>
  inCloud(Effect.gen(function* () {
    const claimOnce = Effect.fn(function* (i: number) {
      const agent = yield* sandbox();
      const claimer = yield* claimSignUp(`Person ${i}`, `person-${i}@example.test`, agent.secret);
      return yield* statementsOf(claimOrganization(claimer.actor, agent.secret));
    });
    // The first claim opens the Store, which makes its tables.
    yield* claimOnce(0);
    const claimed = yield* claimOnce(1);
    expect(claimed.length).toBeLessThanOrEqual(21);
    const { drizzle } = yield* Database;
    for (let i = 0; i < 5; i++) yield* drizzle.execute(sql.raw(`create table claim_cost_${i} (organization_id text)`));
    expect(yield* claimOnce(2)).toHaveLength(claimed.length);
    expect(yield* organizationIds()).toHaveLength(3);
  })));

it.live("the claim's round-trip budget counts the native Store's statements too", () =>
  inCloud(Effect.gen(function* () {
    const claimOnce = Effect.fn(function* (i: number, readsStore: boolean) {
      const agent = yield* sandbox();
      const claimer = yield* claimSignUp(`Person ${i}`, `person-${i}@example.test`, agent.secret);
      const read = readsStore ? readStore(agent.organization.id, { query: "projects" }) : Effect.void;
      return yield* statementsOf(claimOrganization(claimer.actor, agent.secret).pipe(Effect.andThen(read)));
    });
    yield* claimOnce(0, false);
    const claimed = yield* claimOnce(1, false);
    const withStoreRead = yield* claimOnce(2, true);
    expect(withStoreRead.length).toBeGreaterThan(claimed.length);
  })));

// A loose ceiling for what round trips miss; the statement budget above is the tight bound.
it.live("a claim back from the claim page's sign-in has a p95 under 400 ms", () =>
  inCloud(Effect.gen(function* () {
    const warmup = 5;
    const measured = 60;
    const pairs = [];
    for (let i = 0; i < warmup + measured; i++) {
      const agent = yield* sandbox();
      pairs.push({ agent, claimer: yield* claimSignUp(`Person ${i}`, `person-${i}@example.test`, agent.secret) });
    }
    const timings: number[] = [];
    for (const [i, { agent, claimer }] of pairs.entries()) {
      const started = performance.now();
      yield* claimOrganization(claimer.actor, agent.secret);
      if (i >= warmup) timings.push(performance.now() - started);
    }
    expect(yield* organizationIds()).toHaveLength(warmup + measured);
    const sorted = [...timings].sort((a, b) => a - b);
    const at = (q: number) => sorted[Math.ceil(sorted.length * q) - 1] ?? 0;
    console.log(`claim timings ms: p50=${at(0.5).toFixed(1)} p95=${at(0.95).toFixed(1)} max=${at(1).toFixed(1)}`);
    expect(at(0.95)).toBeLessThan(400);
  })), 120_000);
