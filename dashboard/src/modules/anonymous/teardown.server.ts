import "@tanstack/react-start/server-only";
import { and, eq, inArray, notExists, sql } from "drizzle-orm";
import type { ConfigCommand } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { callStore } from "#/modules/config-store/config-store.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { type RemovalConsent, removeTarget } from "#/modules/config-store/store-workspace";
import { isOwnedOnlyByAgents, tokenRuns } from "#/modules/identity/claim-required.server";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { forgetServers } from "#/modules/machines/forget-servers.server";
import { removeOrganization } from "#/modules/organization/organization-removal.server";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { analyticsSalt, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";

const FORGET_AFTER_MS = 3_600_000;

const STUCK_AFTER_MS = 24 * 3_600_000;

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
 * removal asks the Servers first; a Server that stayed unreachable for an hour after that removal began is forgotten
 * so the removal can finish. A claim that lands before any removal keeps everything.
 */
export const tearDownStep = Effect.fn("AnonymousTeardown.step")(function* (organizationId: string) {
  const found = yield* readSandboxState(organizationId);
  if (found.state !== "lapsed") return step(organizationId, found.state, "nothing");
  if (found.paired) yield* forgetStalledServers(organizationId, found.ownerId);
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

const removalStartedAt = Effect.fn("AnonymousTeardown.removalStartedAt")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ started: number | null }>(sql`
    select (extract(epoch from least(
      (select removal_started_at from organization_pairing where organization_id = ${organizationId}),
      (select to_timestamp(min(at)) from config_removal_mark where organization_id = ${organizationId} and state <> 'removed')
    )) * 1000)::double precision as started`, "objects");
  return row?.started ?? null;
});

const forgetStalledServers = Effect.fn("AnonymousTeardown.forgetServers")(function* (organizationId: string, ownerId: string) {
  const started = yield* removalStartedAt(organizationId);
  if (started === null || started + FORGET_AFTER_MS > Date.now()) return;
  const forgotten = yield* forgetServers({ userId: ownerId, organizationId });
  if (forgotten.ok) {
    yield* Effect.logInfo("anonymous_server_forgotten", {
      organizationId, servers: forgotten.value.servers.map((server) => server.id), volumes: forgotten.value.volumes.length,
    });
    return;
  }
  yield* Effect.logInfo("A lapsed sandbox's Servers still answer; its removal keeps waiting on them.", { organizationId, refusal: forgotten.refusal });
  if (started + STUCK_AFTER_MS <= Date.now()) {
    yield* Effect.logWarning("A lapsed sandbox has waited a day on a Server that answers but never confirms its removal.", { organizationId, refusal: forgotten.refusal });
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
    order by id`, "objects");
  return yield* Effect.filter(rows.map((row) => row.id), isOwnedOnlyByAgents);
});
