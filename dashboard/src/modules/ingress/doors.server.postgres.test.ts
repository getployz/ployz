import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { ConfigStore } from "@ployz/sdk";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { closeDoorsRun, closeStaleDoors, bindDoorsRun, executeDoorsOnce, freshSwitchDigest, requestDoorsSwitch } from "#/modules/ingress/doors.server";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { InngestClient } from "#/modules/inngest/client";
import { AppConfig } from "#/server/config.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ORG = "00000000-0000-4000-8000-00000000c2b1";
const ADA = "00000000-0000-4000-8000-00000000c2b2";
const RUN = "run-1";

let harness: PostgresTestHarness;
let session: ScopedRuntimeClientSession = { status: "no_connection" };
const sets: string[][] = [];

/** Servers that hold the doors they are told, unless `held` says what they hold instead. */
const answering = (held: (doors: readonly string[]) => readonly string[] = (doors) => doors): ScopedRuntimeClientSession => ({
  status: "connected",
  connected: asTestDouble<PloyzSession>()({
    setIngressDoors: (doors: readonly string[]) => {
      sets.push([...doors]);
      return Effect.succeed({ machines: [{ server: "s1", doors: held(doors) }], failures: [] });
    },
  }),
});
const runtime = asTestDouble<OrganizationRuntime["Service"]>()({ cancel: () => Effect.void, open: () => Effect.sync(() => session) });

/** A config store with no Environments, so a switch's redeploy has nothing to move. */
const store = { open: Effect.succeed(asTestDouble<ConfigStore>()({ read: async () => ({ projects: [] }) })) };

const inngest = new Inngest({ id: "doors-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

/** The services a switch with no Environments never reaches: the redeploy finds nothing to deploy. */
const unused = {
  config: asTestDouble<AppConfig["Service"]>()({}),
  github: asTestDouble<GithubApi["Service"]>()({}),
};

const run = <A, E>(effect: Effect.Effect<A, E, Database | OrganizationRuntime | CloudStore | InngestClient | AppConfig | GithubApi | SecretEncryption>) =>
  harness.runEffect(effect.pipe(
    Effect.provideService(OrganizationRuntime, runtime),
    Effect.provideService(CloudStore, store),
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(AppConfig, unused.config),
    Effect.provideService(GithubApi, unused.github),
  ));

const setup = (state: "set_up" | "active" | "left") => harness.pool.query(
  "insert into organization_cloudflare_ingress (organization_id, zone, tunnel_id, tunnel_name, state) values ($1, 'example.com', 't1', 'ployz-acme', $2) on conflict (organization_id) do update set state = $2",
  [ORG, state],
);
const attempts = async () => (await harness.pool.query("select id, state, want, stage, failure from ingress_doors_attempt order by requested_at")).rows;
const stateNow = async () => (await harness.pool.query("select state from organization_cloudflare_ingress")).rows[0]?.state;
const outbox = async () => (await harness.pool.query("select count(*)::int as n from operation_outbox")).rows[0]?.n;

/** Ask for a switch and bind a run to it, as the Inngest function does before it executes. */
const asked = async (to: "tunnel" | "public") => {
  const { change } = await run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to }));
  await run(bindDoorsRun({ attemptId: change.attemptId, organizationId: ORG }, RUN));
  return { attemptId: change.attemptId, organizationId: ORG };
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

beforeEach(async () => {
  sets.length = 0;
  session = answering();
  await harness.pool.query(`
    truncate table operation_outbox, "user", organization cascade;
    insert into organization (id, name, slug) values ('${ORG}', 'Acme', 'acme');
    insert into "user" (id, email, name) values ('${ADA}', 'ada@example.test', 'Ada');
  `);
});

it("won't switch to Cloudflare before it is set up", async () => {
  await expect(run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }))).rejects.toMatchObject({ message: "Set up Cloudflare first, then switch to it." });
  expect(await attempts()).toEqual([]);
  expect(await outbox()).toBe(0);
});

it("writes one pending switch with one run, and answers a second request with the first", async () => {
  await setup("set_up");
  const first = await run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }));
  const second = await run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }));
  expect(second.change.attemptId).toBe(first.change.attemptId);
  expect(await attempts()).toHaveLength(1);
  expect(await outbox()).toBe(1);
});

it("opens both doors, flips to Cloudflare, closes the public door, and ends finished", async () => {
  await setup("set_up");
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toEqual({ attemptId: request.attemptId, state: "finished" });
  expect(sets).toEqual([["public", "tunnel"], ["tunnel"]]);
  expect(await stateNow()).toBe("active");
  expect(await attempts()).toMatchObject([{ state: "finished", want: "tunnel", stage: "closing", failure: null }]);
});

it("goes back to public ports the same way and leaves the tunnel as the user's to delete", async () => {
  await setup("active");
  const request = await asked("public");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "finished" });
  expect(sets).toEqual([["public", "tunnel"], ["public"]]);
  expect(await stateNow()).toBe("left");
});

it("moves nothing when a Server doesn't take both doors, and says so", async () => {
  await setup("set_up");
  session = answering(() => ["public"]);
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([["public", "tunnel"]]);
  expect(await stateNow()).toBe("set_up");
  expect(await attempts()).toMatchObject([{ state: "failed", failure: "s1 still has other doors open." }]);
});

it("fails the switch without asking when no Server answers", async () => {
  await setup("set_up");
  session = { status: "unreachable", error: null };
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([]);
  expect(await stateNow()).toBe("set_up");
});

it("closes a retry that finds the switch running without asking the Cluster again", async () => {
  await setup("set_up");
  const request = await asked("tunnel");
  await harness.pool.query("update ingress_doors_attempt set state = 'running', started_at = now()");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([]);
  expect(await attempts()).toMatchObject([{ failure: "Cloud lost track of the switch. Check the section and switch again." }]);
});

it("ends a switch whose run failed or was cancelled, once", async () => {
  await setup("set_up");
  const request = await asked("tunnel");
  expect(await run(closeDoorsRun(RUN, "cancellation", request.attemptId))).toBe(1);
  expect(await run(closeDoorsRun(RUN, "cancellation", request.attemptId))).toBe(0);
  expect(await attempts()).toMatchObject([{ state: "failed", failure: "The switch was cancelled before it started." }]);
});

it("sweeps a request nobody picked up and a switch that outlived its run", async () => {
  await setup("set_up");
  await asked("tunnel");
  await harness.pool.query("update ingress_doors_attempt set requested_at = now() - interval '1 hour'");
  expect(await run(closeStaleDoors())).toEqual({ unstarted: 1, lost: 0 });
  await harness.pool.query("truncate table ingress_doors_attempt");
  await asked("tunnel");
  await harness.pool.query("update ingress_doors_attempt set state = 'running', started_at = now() - interval '1 hour'");
  expect(await run(closeStaleDoors())).toEqual({ unstarted: 0, lost: 1 });
  expect(await attempts()).toMatchObject([{ state: "failed" }]);
});

it("drops a pending approval for a switch the Cluster has already made", async () => {
  await setup("active");
  const stored = "digest";
  expect(await run(freshSwitchDigest(ORG, stored, { to: "tunnel" }))).toBeNull();
  expect(await run(freshSwitchDigest(ORG, stored, { unrelated: true }))).toBe(stored);
});
