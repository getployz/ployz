import "@tanstack/react-start/server-only";
import { and, eq, inArray, notExists, sql } from "drizzle-orm";
import type { ConfigCommand } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { anonymousTeardown } from "#/modules/anonymous/tables";
import { callStore } from "#/modules/config-store/config-store.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { type RemovalConsent, removeTarget } from "#/modules/config-store/store-workspace";
import { isOwnedOnlyByAgents, tokenRuns } from "#/modules/identity/claim-required.server";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { checkForgetServers, forgetServers } from "#/modules/machines/forget-servers.server";
import { removeOrganization } from "#/modules/organization/organization-removal.server";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { analyticsSalt, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";

const CONSENT_ROUNDS = 3;

/**
 * What one run finds. `claimed` is an Organization a person owns, alone or alongside the agent; `live` one whose
 * agent's token still runs; `lapsed` one past that token, with its first anonymous owner (who removes on Cloud's
 * behalf), its slug, whether a Cluster is paired, and the Projects the Store still lists.
 */
export type SandboxState =
  | { state: "gone" }
  | { state: "claimed" }
  | { state: "live" }
  | { state: "lapsed"; ownerId: string; slug: string; paired: boolean; projects: string[] };

export type TeardownDid = "nothing" | "removed_projects" | "waited_on_servers" | "removed_organization";

export type TeardownStep = { organizationId: string; state: SandboxState["state"]; did: TeardownDid };

const step = (organizationId: string, state: SandboxState["state"], did: TeardownDid): TeardownStep => ({ organizationId, state, did });

const lockedOrganizationState = Effect.fn("AnonymousTeardown.lockedState")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ slug: organization.slug }).from(organization)
    .where(eq(organization.id, organizationId)).for("update");
  if (row === undefined) return { state: "gone" } as const;
  if (!(yield* isOwnedOnlyByAgents(organizationId))) return { state: "claimed" } as const;
  const [owner] = yield* drizzle.select({ userId: member.userId }).from(member)
    .innerJoin(user, eq(user.id, member.userId))
    .where(and(eq(member.organizationId, organizationId), eq(member.role, "owner"), eq(user.isAnonymous, true)))
    .orderBy(member.createdAt).limit(1);
  if (owner === undefined) return { state: "claimed" } as const;
  const [running] = yield* drizzle.select({ id: organizationToken.id }).from(organizationToken)
    .innerJoin(member, and(eq(member.organizationId, organizationToken.organizationId), eq(member.userId, organizationToken.userId)))
    .innerJoin(user, eq(user.id, member.userId))
    .where(and(
      eq(organizationToken.organizationId, organizationId), eq(member.role, "owner"), eq(user.isAnonymous, true), tokenRuns,
    )).limit(1);
  if (running !== undefined) return { state: "live" } as const;
  const [pairing] = yield* drizzle.select({ organizationId: organizationPairing.organizationId }).from(organizationPairing)
    .where(eq(organizationPairing.organizationId, organizationId));
  return { state: "lapsed", ownerId: owner.userId, slug: row.slug, paired: pairing !== undefined } as const;
});

const readOrganizationState = Effect.fn("AnonymousTeardown.readOrganizationState")(function* (organizationId: string) {
  const database = yield* Database;
  return yield* database.transaction(lockedOrganizationState(organizationId)).pipe(Effect.orDie);
});

const keptStep = Effect.fn("AnonymousTeardown.keptStep")(function* (organizationId: string) {
  const again = yield* readOrganizationState(organizationId);
  return again.state === "lapsed" ? null : step(organizationId, again.state, "nothing");
});

/** Where Organization `organizationId` stands, with the Projects its Store still lists when it lapsed. */
export const readSandboxState = Effect.fn("AnonymousTeardown.readState")(function* (organizationId: string) {
  const found = yield* readOrganizationState(organizationId);
  if (found.state !== "lapsed") return found;
  const store = yield* cloudStore;
  const listed = yield* storeTry(() => store.read(organizationId, { query: "projects" })).pipe(Effect.orDie);
  const lapsed: SandboxState = { ...found, projects: listed.projects.map((project) => project.name) };
  return lapsed;
});

/**
 * One step towards an empty seat, re-deriving the state every time so a retry picks up where a crash left off: a
 * lapsed sandbox loses its Projects, then its Organization with every row that still names it and its agent. Each
 * removal asks the Servers first. When they hold it up, Cloud probes them; once every probe for an hour since the last
 * answer reached none, Cloud forgets them and tries again. A claim that lands before any removal keeps everything.
 */
export const tearDownStep = Effect.fn("AnonymousTeardown.step")(function* (organizationId: string) {
  const found = yield* readSandboxState(organizationId);
  if (found.state !== "lapsed") return step(organizationId, found.state, "nothing");
  const tried = yield* removeSandbox(organizationId, found).pipe(Effect.catchTag("StoreRefused", Effect.succeed));
  if (!(tried instanceof StoreRefused) && tried.did !== "waited_on_servers") return tried;
  if (yield* waitOnServers(organizationId)) {
    const again = yield* readSandboxState(organizationId);
    if (again.state !== "lapsed") return step(organizationId, again.state, "nothing");
    return yield* removeSandbox(organizationId, again);
  }
  if (tried instanceof StoreRefused) return yield* tried;
  return tried;
});

const removeSandbox = Effect.fn("AnonymousTeardown.removeSandbox")(function* (
  organizationId: string,
  found: Extract<SandboxState, { state: "lapsed" }>,
) {
  if (found.projects.length > 0) {
    for (const project of found.projects) {
      const kept = yield* keptStep(organizationId);
      if (kept) return kept;
      yield* removeProject(organizationId, project);
    }
    const store = yield* cloudStore;
    const left = yield* storeTry(() => store.read(organizationId, { query: "projects" })).pipe(Effect.orDie);
    return step(organizationId, "lapsed", left.projects.length === 0 ? "removed_projects" : "waited_on_servers");
  }
  const kept = yield* keptStep(organizationId);
  if (kept) return kept;
  const caller = { userId: found.ownerId, organization: { id: organizationId, slug: found.slug }, role: "owner" as const };
  const removed = yield* removeOrganization(caller, found.slug, deletingSandbox(organizationId)).pipe(
    Effect.catchTag("SandboxKept", (kept) => Effect.succeed(step(organizationId, kept.state, "nothing"))),
  );
  if ("did" in removed) return removed;
  if (!removed.ok) return yield* new StoreRefused(removed.refusal);
  if (!removed.value.removed) {
    yield* Effect.logInfo("A lapsed sandbox waits on its Servers before its Organization goes.", { organizationId, servers: removed.value.servers });
    return step(organizationId, "lapsed", "waited_on_servers");
  }
  return step(organizationId, "lapsed", "removed_organization");
});

/**
 * Probes the Servers a lapsed sandbox's removal waits on and reports whether Cloud forgot them. The database clock
 * times it all: silence starts at the first probe that reaches none of them and ends at any answer, and Cloud forgets
 * only once that silence has lasted an hour. A sandbox that has waited a day on Servers that answer warns once.
 */
const waitOnServers = Effect.fn("AnonymousTeardown.waitOnServers")(function* (organizationId: string) {
  const database = yield* Database;
  const waiting = yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const found = yield* lockedOrganizationState(organizationId);
    if (found.state !== "lapsed" || !found.paired) return null;
    yield* drizzle.insert(anonymousTeardown).values({ organizationId }).onConflictDoNothing();
    const [row] = yield* drizzle.select({
      due: sql<boolean>`coalesce(${anonymousTeardown.silentSince} <= clock_timestamp() - interval '1 hour', false)`,
      stuck: sql<boolean>`${anonymousTeardown.waitingSince} <= clock_timestamp() - interval '24 hours' and ${anonymousTeardown.warnedAt} is null`,
    }).from(anonymousTeardown).where(eq(anonymousTeardown.organizationId, organizationId));
    return row === undefined ? null : { ownerId: found.ownerId, ...row };
  })).pipe(Effect.orDie);
  if (waiting === null) return false;
  const { drizzle } = yield* Database;
  const forgetter = { userId: waiting.ownerId, organizationId };
  const probed = waiting.due ? yield* forgetServers(forgetter) : yield* checkForgetServers(forgetter);
  if (probed.ok) {
    yield* drizzle.update(anonymousTeardown).set({ silentSince: sql`coalesce(${anonymousTeardown.silentSince}, clock_timestamp())` })
      .where(eq(anonymousTeardown.organizationId, organizationId)).pipe(Effect.orDie);
    if (!waiting.due) return false;
    yield* Effect.logInfo("anonymous_server_forgotten", {
      organizationId, servers: probed.value.servers.map((server) => server.id), volumes: probed.value.volumes.length,
    });
    return true;
  }
  yield* drizzle.update(anonymousTeardown).set({ silentSince: null })
    .where(eq(anonymousTeardown.organizationId, organizationId)).pipe(Effect.orDie);
  yield* Effect.logInfo("A lapsed sandbox's removal waits on its Servers.", { organizationId, refusal: probed.refusal });
  if (waiting.stuck) {
    yield* Effect.logWarning("A lapsed sandbox has waited a day on a Server that answers but never confirms its removal.", { organizationId, refusal: probed.refusal });
    yield* drizzle.update(anonymousTeardown).set({ warnedAt: sql`clock_timestamp()` })
      .where(eq(anonymousTeardown.organizationId, organizationId)).pipe(Effect.orDie);
  }
  return false;
});

class SandboxKept extends Data.TaggedError("SandboxKept")<{ readonly state: Exclude<SandboxState["state"], "lapsed"> }> {}

const deletingSandbox = (organizationId: string) => (drop: Effect.Effect<void, never, Database>) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const found = yield* lockedOrganizationState(organizationId);
  if (found.state !== "lapsed") return yield* new SandboxKept({ state: found.state });
  const agents = yield* drizzle.select({ userId: member.userId }).from(member)
    .innerJoin(user, eq(user.id, member.userId))
    .where(and(eq(member.organizationId, organizationId), eq(user.isAnonymous, true)));
  yield* drizzle.delete(analyticsSite).where(eq(analyticsSite.organizationId, organizationId));
  yield* drizzle.delete(analyticsSalt).where(eq(analyticsSalt.organizationId, organizationId));
  yield* drop;
  if (agents.length > 0) {
    yield* drizzle.delete(user).where(and(
      inArray(user.id, agents.map((agent) => agent.userId)),
      notExists(drizzle.select({ one: sql`1` }).from(member).where(eq(member.userId, user.id))),
    ));
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
    const answer = yield* Effect.tryPromise({ try: () => removeTarget(commit, { project, environment: null }, consent), catch: (cause) => cause }).pipe(
      Effect.catch((cause) => cause instanceof StoreRefused ? Effect.fail(cause) : Effect.die(cause)),
    );
    if ("sent" in answer) return answer.sent;
    consent = answer.ask;
  }
  return yield* new StoreRefused({ code: "conflict", message: `Removing ${project} kept asking for consent.`, details: { project } });
});

/** Organizations an anonymous agent alone owns whose every agent token has lapsed: the sweep's work list. */
export const listLapsedOrganizationIds = Effect.fn("AnonymousTeardown.listLapsed")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.execute<{ id: string }>(sql`
    select distinct m.organization_id as id
    from member m
    join "user" u on u.id = m.user_id
    where m.role = 'owner' and u.is_anonymous
      and not exists (
        select 1 from organization_token
        join member tm on tm.organization_id = organization_token.organization_id and tm.user_id = organization_token.user_id and tm.role = 'owner'
        join "user" tu on tu.id = organization_token.user_id
        where organization_token.organization_id = m.organization_id and tu.is_anonymous and ${tokenRuns})
      and not exists (
        select 1 from member hm
        join "user" hu on hu.id = hm.user_id
        where hm.organization_id = m.organization_id and hm.role = 'owner' and not hu.is_anonymous)
    order by id`, "objects");
  return rows.map((row) => row.id);
});
