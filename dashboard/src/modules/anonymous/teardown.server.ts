import "@tanstack/react-start/server-only";
import { and, eq, notExists, sql } from "drizzle-orm";
import type { ConfigCommand } from "@ployz/sdk";
import { Effect } from "effect";
import { callStore } from "#/modules/config-store/config-store.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { type RemovalConsent, removeTarget } from "#/modules/config-store/store-workspace";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { forgetServers } from "#/modules/machines/forget-servers.server";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { removeOrganization } from "#/modules/organization/organization-removal.server";
import { organization, organizationChange } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { uptimeIncident, uptimeResult, uptimeResultHourly } from "#/modules/uptime/tables";
import { analyticsSalt, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";

const FORGET_AFTER_MS = 3_600_000;

const CONSENT_ROUNDS = 3;

/**
 * What one run finds. `claimed` is an Organization no anonymous agent owns any more; `live` one whose agent's token
 * still runs; `lapsed` one past that token, with its first anonymous owner (who removes on Cloud's behalf), its slug,
 * when the token lapsed, whether a Cluster is paired, and the Projects the Store still lists.
 */
export type SandboxState =
  | { state: "gone" }
  | { state: "claimed" }
  | { state: "live" }
  | { state: "lapsed"; ownerId: string; slug: string; expiresAt: Date | null; paired: boolean; projects: string[] };

export type TeardownDid = "nothing" | "removed_projects" | "waited_on_servers" | "removed_organization" | "cleaned_up";

export type TeardownStep = { organizationId: string; state: SandboxState["state"]; did: TeardownDid };

/** Where Organization `organizationId` stands, read under its row lock so a claim and a teardown never interleave. */
export const readSandboxState = Effect.fn("AnonymousTeardown.readState")(function* (organizationId: string) {
  const database = yield* Database;
  const { drizzle } = database;
  const found = yield* database.transaction(Effect.gen(function* () {
    const [row] = yield* drizzle.select({ slug: organization.slug }).from(organization)
      .where(eq(organization.id, organizationId)).for("update");
    if (row === undefined) return { state: "gone" } as const;
    const owners = yield* drizzle.select({ userId: member.userId }).from(member)
      .innerJoin(user, eq(user.id, member.userId))
      .where(and(eq(member.organizationId, organizationId), eq(member.role, "owner"), eq(user.isAnonymous, true)))
      .orderBy(member.createdAt);
    const [owner] = owners;
    if (owner === undefined) return { state: "claimed" } as const;
    const tokens = yield* drizzle.select({ expiresAt: organizationToken.expiresAt }).from(organizationToken)
      .innerJoin(member, and(eq(member.organizationId, organizationToken.organizationId), eq(member.userId, organizationToken.userId)))
      .innerJoin(user, eq(user.id, member.userId))
      .where(and(eq(organizationToken.organizationId, organizationId), eq(member.role, "owner"), eq(user.isAnonymous, true)));
    const expiresAt = tokens.reduce<Date | null>((latest, token) => latest === null || token.expiresAt > latest ? token.expiresAt : latest, null);
    if (expiresAt !== null && expiresAt.getTime() > Date.now()) return { state: "live" } as const;
    const pairings = yield* drizzle.select({ organizationId: organizationPairing.organizationId }).from(organizationPairing)
      .where(eq(organizationPairing.organizationId, organizationId));
    return { state: "lapsed", ownerId: owner.userId, slug: row.slug, expiresAt, paired: pairings.length > 0 } as const;
  })).pipe(Effect.orDie);
  if (found.state !== "lapsed") return found;
  const store = yield* cloudStore;
  const listed = yield* storeTry(() => store.read(organizationId, { query: "projects" })).pipe(Effect.orDie);
  const lapsed: SandboxState = { ...found, projects: listed.projects.map((project) => project.name) };
  return lapsed;
});

/**
 * One step towards an empty seat, re-deriving the state every time so a retry picks up where a crash left off: a
 * lapsed sandbox loses its Projects, then its Organization, then every row that still names it and its agent; a
 * Server that stopped answering an hour past the token is forgotten first so the Organization can go.
 */
export const tearDownStep = Effect.fn("AnonymousTeardown.step")(function* (organizationId: string) {
  const found = yield* readSandboxState(organizationId);
  const step = (did: TeardownDid): TeardownStep => ({ organizationId, state: found.state, did });
  if (found.state === "gone") {
    yield* cleanUp(organizationId);
    return step("cleaned_up");
  }
  if (found.state !== "lapsed") return step("nothing");
  if (found.paired && (found.expiresAt === null || found.expiresAt.getTime() + FORGET_AFTER_MS <= Date.now())) {
    yield* forgetUnreachableServers(organizationId, found.ownerId);
  }
  if (found.projects.length > 0) {
    for (const project of found.projects) yield* removeProject(organizationId, project);
    const store = yield* cloudStore;
    const left = yield* storeTry(() => store.read(organizationId, { query: "projects" })).pipe(Effect.orDie);
    return step(left.projects.length === 0 ? "removed_projects" : "waited_on_servers");
  }
  const removed = yield* removeOrganization({ userId: found.ownerId, organization: { id: organizationId, slug: found.slug }, role: "owner" }, found.slug);
  if (!removed.ok) return yield* new StoreRefused(removed.refusal);
  if (!removed.value.removed) {
    yield* Effect.logInfo("A lapsed sandbox waits on its Servers before its Organization goes.", { organizationId, servers: removed.value.servers });
    return step("waited_on_servers");
  }
  yield* cleanUp(organizationId);
  return step("removed_organization");
});

const forgetUnreachableServers = Effect.fn("AnonymousTeardown.forgetServers")(function* (organizationId: string, ownerId: string) {
  const forgotten = yield* forgetServers({ userId: ownerId, organizationId });
  if (forgotten.ok) {
    yield* Effect.logInfo("anonymous_server_forgotten", {
      organizationId, servers: forgotten.value.servers.map((server) => server.id), volumes: forgotten.value.volumes.length,
    });
  } else {
    yield* Effect.logWarning("A lapsed sandbox's Servers can't be forgotten yet.", { organizationId, refusal: forgotten.refusal });
  }
});

const removeProject = Effect.fn("AnonymousTeardown.removeProject")(function* (organizationId: string, project: string) {
  const run = Effect.runPromiseWith(yield* Effect.context<Effect.Services<ReturnType<typeof callStore>>>());
  const commit = async (command: ConfigCommand) => {
    const answer = await run(callStore(organizationId, null, { operation: "write", command }, { source: "dashboard" }, { approval: "not_required" }));
    if (answer.ok) return answer.value;
    throw new StoreRefused(answer.refusal);
  };
  let consent: RemovalConsent = { accept: {}, version: null, everyCopy: {} };
  for (let round = 0; round < CONSENT_ROUNDS; round++) {
    const answer = yield* Effect.promise(() => removeTarget(commit, { project, environment: null }, consent));
    if ("sent" in answer) return answer.sent;
    consent = answer.ask;
  }
  return yield* new StoreRefused({ code: "conflict", message: `Removing ${project} kept asking for consent.`, details: { project } });
});

const cleanUp = Effect.fn("AnonymousTeardown.cleanUp")(function* (organizationId: string) {
  const database = yield* Database;
  const { drizzle } = database;
  yield* database.transaction(Effect.gen(function* () {
    yield* drizzle.delete(observeIngestCursor).where(eq(observeIngestCursor.organizationId, organizationId));
    yield* drizzle.delete(crashEvent).where(eq(crashEvent.organizationId, organizationId));
    yield* drizzle.delete(uptimeIncident).where(eq(uptimeIncident.organizationId, organizationId));
    yield* drizzle.delete(uptimeResult).where(eq(uptimeResult.organizationId, organizationId));
    yield* drizzle.delete(uptimeResultHourly).where(eq(uptimeResultHourly.organizationId, organizationId));
    yield* drizzle.delete(analyticsSite).where(eq(analyticsSite.organizationId, organizationId));
    yield* drizzle.delete(analyticsSalt).where(eq(analyticsSalt.organizationId, organizationId));
    yield* drizzle.delete(organizationChange).where(eq(organizationChange.organizationId, organizationId));
    yield* deleteOrphanedAnonymousUsers;
  })).pipe(Effect.orDie);
});

const deleteOrphanedAnonymousUsers = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(user).where(and(
    eq(user.isAnonymous, true),
    notExists(drizzle.select({ one: sql`1` }).from(member).where(eq(member.userId, user.id))),
  ));
});

/** Organizations an anonymous agent owns whose every agent token has lapsed: the sweep's work list. */
export const listLapsedOrganizationIds = Effect.fn("AnonymousTeardown.listLapsed")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ id: string }>(sql`
    select distinct m.organization_id as id
    from member m
    join "user" u on u.id = m.user_id
    where m.role = 'owner' and u.is_anonymous
      and not exists (
        select 1 from organization_token t
        join member tm on tm.organization_id = t.organization_id and tm.user_id = t.user_id and tm.role = 'owner'
        join "user" tu on tu.id = t.user_id
        where t.organization_id = m.organization_id and tu.is_anonymous and t.expires_at > now())
    order by id`, "objects");
  return rows.map((row) => row.id);
});
