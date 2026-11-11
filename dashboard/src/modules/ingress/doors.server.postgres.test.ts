import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { ConfigStore } from "@ployz/sdk";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { endClusterIngress } from "#/modules/cloudflare/cluster-gone.server";
import { closeDoorsRun, closeStaleDoors, bindDoorsRun, executeDoorsOnce, freshSwitchDigest, requestDoorsSwitch, reviewSwitch } from "#/modules/ingress/doors.server";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { GithubAuthority } from "#/modules/github/github-authority.server";
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

/** A Cluster publishing no hostname, so a switch finds nothing to move and starts nothing. */
const EMPTY_FRAME = { machines: [], services: [], containers: [], incomplete_ids: { machines: [], containers: [], certificates: [] }, unanswered: [] };
/** A look that missed a Server: what it publishes is unknown. */
const PARTIAL_FRAME = { ...EMPTY_FRAME, incomplete_ids: { machines: ["m2"], containers: [], certificates: [] } };
/** A Server taking web traffic, as the frame lists it. */
const server = (id: string, membership: string) => ({ machine: { id, name: id, accepts_ingress: true, runtime: { running_builds: 0 } }, membership });
/** A look where a Server stopped answering the probes: its rows are as it last reported them, so what it serves now is unknown. */
const SILENT_FRAME = { ...EMPTY_FRAME, machines: [server("m1", "up"), server("m2", "up")], unanswered: ["m2"] };
/** A look where gossip reads a Server down. */
const DOWN_FRAME = { ...EMPTY_FRAME, machines: [server("m1", "up"), server("m2", "down")] };

/** Servers that hold the doors they are told, unless `held` says what they hold instead, showing `frame` when watched. */
const answering = (held: (doors: readonly string[]) => readonly string[] = (doors) => doors, frame: Effect.Effect<unknown, Error> = Effect.succeed(EMPTY_FRAME)): ScopedRuntimeClientSession => ({
  status: "connected",
  connected: asTestDouble<PloyzSession>()({
    watchFirstFrame: () => frame,
    setIngressDoors: (doors: readonly string[]) => {
      sets.push([...doors]);
      return Effect.succeed({ machines: [{ server: "s1", doors: held(doors) }], failures: [] });
    },
  }),
});
const runtime = asTestDouble<OrganizationRuntime["Service"]>()({ cancel: () => Effect.void, open: () => Effect.sync(() => session) });

/** What the Store double answers a read with: the rows a switch looks at, and nothing for anything else. */
type StoreRows =
  | { namespaces: { namespace: string; project: string; environment: string }[] }
  | { namespace: string; services: Record<string, string>; routes: { kind: "custom" | "generated"; hostname: string; service: string }[] }
  | { domains: { kind: "custom" | "generated"; hostname: string; service: string }[] }
  | { projects: never[] };
/** The Store as a switch reads it: no Environment owns a Namespace unless a test says so, and a switch never writes to it. */
const noOwners = ({ query }: { query: string }): StoreRows => query === "namespaces" ? { namespaces: [] } : { projects: [] };
let storeViews = noOwners;
/** Held by a test that pauses a switch inside a Store read. */
let storeRead: () => Promise<void> = () => Promise.resolve();
const store = { open: Effect.succeed(asTestDouble<ConfigStore>()({ read: async (_organizationId: string, query: { query: string }) => { await storeRead(); return storeViews(query); } })) };

const inngest = new Inngest({ id: "doors-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

/** The services a switch with no Environments never reaches: the redeploy finds nothing to deploy. */
const unused = {
  config: asTestDouble<AppConfig["Service"]>()({}),
  github: asTestDouble<GithubApi["Service"]>()({}),
  authority: asTestDouble<GithubAuthority["Service"]>()({}),
};

const run = <A, E>(effect: Effect.Effect<A, E, Database | OrganizationRuntime | CloudStore | InngestClient | AppConfig | GithubApi | GithubAuthority | SecretEncryption>) =>
  harness.runEffect(effect.pipe(
    Effect.provideService(OrganizationRuntime, runtime),
    Effect.provideService(CloudStore, store),
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(AppConfig, unused.config),
    Effect.provideService(GithubApi, unused.github),
    Effect.provideService(GithubAuthority, unused.authority),
  ));

const setup = (state: "set_up" | "active" | "left" | "setting_up") => harness.pool.query(
  "insert into organization_cloudflare_ingress (organization_id, zone, tunnel_id, tunnel_name, state) values ($1, 'example.com', $3, 'ployz-acme', $2) on conflict (organization_id) do update set state = $2, tunnel_id = $3",
  [ORG, state, state === "setting_up" ? null : "t1"],
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
  storeViews = noOwners;
  storeRead = () => Promise.resolve();
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

it("writes one pending switch with one run, joins a second request the same way, and refuses one the other way", async () => {
  await setup("set_up");
  const first = await run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }));
  expect(first.joined).toBe(false);
  const second = await run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }));
  expect(second).toEqual({ change: first.change, joined: true });
  await expect(run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "public" })))
    .rejects.toMatchObject({ message: "A switch to Cloudflare is under way. Wait for it to finish, then switch again." });
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

/** The first Server joined on an instruction founding the Cluster with every Door closed, as Cloudflare's first-run card asks for. */
const foundedClosed = () => harness.pool.query(`
  with fleet as (
    insert into fleet_token (organization_id, name, token_hash, expires_at, founds_closed, created_by_user_id)
    values ($1, 'fleet', 'hash', now() + interval '1 day', true, $2) returning id
  ), waiting as (
    insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip)
    select $1, id, 'key', '{}'::jsonb, '203.0.113.9' from fleet returning id, fleet_token_id
  )
  insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state)
  select $1, id, fleet_token_id, 'founder', '{"ingressClosed": true}'::jsonb, gen_random_uuid(), 'joined' from waiting
`, [ORG, ADA]);
const CLOSED_FRAME = { ...EMPTY_FRAME, machines: [server("m1", "up")] };

it("switches a Cluster founded closed with only the tunnel, never the ports, and flips to Cloudflare once it has acted", async () => {
  await setup("set_up");
  await foundedClosed();
  session = answering((doors) => doors, Effect.succeed(CLOSED_FRAME));
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toEqual({ attemptId: request.attemptId, state: "finished" });
  expect(sets).toEqual([["tunnel"], ["tunnel"]]);
  expect(await stateNow()).toBe("active");
});

it("leaves a Cluster founded closed set up, with the switch free to ask again, when its Servers aren't all seen", async () => {
  await setup("set_up");
  await foundedClosed();
  session = answering((doors) => doors, Effect.succeed({ ...CLOSED_FRAME, incomplete_ids: { machines: ["m2"], containers: [], certificates: [] } }));
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toEqual({ attemptId: request.attemptId, state: "failed" });
  expect(sets).toEqual([]);
  expect(await stateNow()).toBe("set_up");
  expect(await attempts()).toMatchObject([{ state: "failed", failure: "Some servers didn't answer. Switch again once they all do." }]);

  session = answering((doors) => doors, Effect.succeed(CLOSED_FRAME));
  const again = await run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }));
  expect(again.joined).toBe(false);
  await run(bindDoorsRun({ attemptId: again.change.attemptId, organizationId: ORG }, "run-2"));
  expect(await run(executeDoorsOnce({ attemptId: again.change.attemptId, organizationId: ORG }, "run-2"))).toMatchObject({ state: "finished" });
  expect(sets).toEqual([["tunnel"], ["tunnel"]]);
  expect(await stateNow()).toBe("active");
});

it("finishes a switch whose last step failed when asked again, and refuses one already made", async () => {
  await setup("active");
  await harness.pool.query(
    "insert into ingress_doors_attempt (id, organization_id, want, state, stage, failure, requested_at, ended_at) values (gen_random_uuid(), $1, 'tunnel', 'failed', 'closing', 's1 didn''t switch.', now() - interval '1 minute', now())",
    [ORG],
  );
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "finished" });
  expect(sets).toEqual([["public", "tunnel"], ["tunnel"]]);
  expect(await stateNow()).toBe("active");
  await expect(run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }))).rejects.toMatchObject({ message: "Your servers already use Cloudflare Tunnel." });
});

it("goes back to public ports the same way and leaves the tunnel as the user's to delete", async () => {
  await setup("active");
  const request = await asked("public");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "finished" });
  expect(sets).toEqual([["public", "tunnel"], ["public"]]);
  expect(await stateNow()).toBe("left");
});

const hosted = () => harness.pool.query(
  "insert into organization_cluster_domain (organization_id, endpoint, name, encrypted_token, reserved_at, lease_renewed_at) values ($1, 'https://dns.example.test/', 'acme.ployz.test', '{}', now(), now())",
  [ORG],
);
/** A container of shop-prod serving `hostname`, as the frame lists it. */
const serving = (hostname: string) => ({ kind: "service_container", namespace: "shop-prod", resolved_spec: { name: "shop", ports: [{ mode: "ingress", hostname }] } });
type Route = { kind: "custom" | "generated"; hostname: string };
/** The Store with shop/production owning shop-prod: its Applied State serves `routes`, its Working State lists `domains`. */
const owning = (routes: Route[], domains: Route[] = routes): typeof storeViews => ({ query }) => query === "namespaces"
  ? { namespaces: [{ namespace: "shop-prod", project: "shop", environment: "production" }] }
  : query === "namespace"
  ? { namespace: "shop-prod", services: { shop: "shop" }, routes: routes.map((route) => ({ ...route, service: "shop" })) }
  : query === "domains"
  ? { domains: domains.map((route) => ({ ...route, service: "shop" })) }
  : noOwners({ query });

it("leaves a custom domain spelled like a generated name where it is on the way back, and names it in the review", async () => {
  await setup("active");
  await hosted();
  session = answering((doors) => doors, Effect.succeed({ ...EMPTY_FRAME, containers: [serving("shop.example.com")] }));
  storeViews = owning([{ kind: "custom", hostname: "shop.example.com" }]);
  expect(await run(reviewSwitch(ORG, "public"))).toEqual({ moves: [], kept: ["shop.example.com"], zone: "example.com", hostedName: "acme.ployz.test", gap: null });
  const request = await asked("public");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "finished" });
  expect(sets).toEqual([["public", "tunnel"], ["public"]]);
  expect(await stateNow()).toBe("left");
});

it("classifies a published name by the Deployment that put it there, not by Working State: a custom domain removed there but still deployed stays", async () => {
  await setup("active");
  await hosted();
  session = answering((doors) => doors, Effect.succeed({ ...EMPTY_FRAME, containers: [serving("shop.example.com"), serving("shop.example.org")] }));
  storeViews = owning([{ kind: "custom", hostname: "shop.example.com" }, { kind: "custom", hostname: "shop.example.org" }], []);
  expect(await run(reviewSwitch(ORG, "public"))).toEqual({ moves: [], kept: ["shop.example.com", "shop.example.org"], zone: "example.com", hostedName: "acme.ployz.test", gap: null });
  const request = await asked("public");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "finished" });
  expect(sets).toEqual([["public", "tunnel"], ["public"]]);
  expect(await stateNow()).toBe("left");
});

it("stops before the source moves on a name under the zone that no Deployment accounts for, and says so in the review", async () => {
  await setup("active");
  await hosted();
  session = answering((doors) => doors, Effect.succeed({ ...EMPTY_FRAME, containers: [serving("stray.example.com")] }));
  storeViews = owning([]);
  const gap = "stray.example.com is served by shop-prod/shop, which Ployz Cloud can't match to a deployment. Deploy it again or remove it, then switch again.";
  expect(await run(reviewSwitch(ORG, "public"))).toEqual({ moves: [], kept: [], zone: "example.com", hostedName: "acme.ployz.test", gap });
  const request = await asked("public");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([["public", "tunnel"]]);
  expect(await stateNow()).toBe("active");
  expect(await attempts()).toMatchObject([{ state: "failed", stage: "moving", failure: gap }]);
});

it("stops before any Door opens when a generated name would spell a custom domain under the zone, and says so in the review", async () => {
  await setup("set_up");
  await hosted();
  session = answering((doors) => doors, Effect.succeed({ ...EMPTY_FRAME, containers: [serving("wk.acme.ployz.test"), serving("wk.example.com")] }));
  storeViews = owning([{ kind: "generated", hostname: "wk.acme.ployz.test" }, { kind: "custom", hostname: "wk.example.com" }]);
  const gap = "wk.example.com would be the generated domain of shop-prod/shop, and a custom domain already holds it. Remove the generated domain or the custom domain, deploy, then switch again.";
  expect(await run(reviewSwitch(ORG, "tunnel"))).toEqual({
    moves: [{ from: "wk.acme.ployz.test", to: "wk.example.com" }], kept: ["wk.example.com"], zone: "example.com", hostedName: "acme.ployz.test", gap,
  });
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([]);
  expect(await stateNow()).toBe("set_up");
  expect(await attempts()).toMatchObject([{ state: "failed", failure: gap }]);
});

it("opens no Door when a switch to public ports would spell a generated name like a custom domain, so the tunnel is never left beside open ports", async () => {
  await setup("active");
  await hosted();
  session = answering((doors) => doors, Effect.succeed({ ...EMPTY_FRAME, containers: [serving("wk.example.com"), serving("wk.acme.ployz.test")] }));
  storeViews = owning([{ kind: "generated", hostname: "wk.example.com" }, { kind: "custom", hostname: "wk.acme.ployz.test" }]);
  const gap = "wk.acme.ployz.test would be the generated domain of shop-prod/shop, and a custom domain already holds it. Remove the generated domain or the custom domain, deploy, then switch again.";
  expect((await run(reviewSwitch(ORG, "public"))).gap).toBe(gap);
  const request = await asked("public");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([]);
  expect(await stateNow()).toBe("active");
  expect(await attempts()).toMatchObject([{ state: "failed", failure: gap }]);
});

it("moves nothing when a Server doesn't take both doors, and says so", async () => {
  await setup("set_up");
  session = answering(() => ["public"]);
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([["public", "tunnel"]]);
  expect(await stateNow()).toBe("set_up");
  expect(await attempts()).toMatchObject([{ state: "failed", failure: "s1 didn't switch." }]);
});

it("stops with both doors open, before the source moves, when the Servers show no frame", async () => {
  await setup("set_up");
  session = answering((doors) => doors, Effect.fail(new Error("no frame in time")));
  const request = await asked("tunnel");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([["public", "tunnel"]]);
  expect(await stateNow()).toBe("set_up");
  expect(await attempts()).toMatchObject([{ state: "failed", stage: "moving", failure: "Your servers didn't answer. Try again once they do." }]);
});

it("stops the same way when the frame missed a Server, since it may publish an address that would be left behind", async () => {
  await setup("active");
  session = answering((doors) => doors, Effect.succeed(PARTIAL_FRAME));
  const request = await asked("public");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([["public", "tunnel"]]);
  expect(await stateNow()).toBe("active");
  expect(await attempts()).toMatchObject([{ state: "failed", failure: "Some servers didn't answer, so an address may be missing. Try again once they do." }]);
});

it.each([["stopped answering", SILENT_FRAME], ["reads down", DOWN_FRAME]])("stops the same way when a Server %s, since its rows are as it last reported them", async (_, frame) => {
  await setup("active");
  session = answering((doors) => doors, Effect.succeed(frame));
  const request = await asked("public");
  expect(await run(executeDoorsOnce(request, RUN))).toMatchObject({ state: "failed" });
  expect(sets).toEqual([["public", "tunnel"]]);
  expect(await stateNow()).toBe("active");
  expect(await attempts()).toMatchObject([{ state: "failed", failure: "Some servers didn't answer, so an address may be missing. Try again once they do." }]);
});

it("refuses a switch either way while a setup holds the Cluster, and lets one through once the setup went stale", async () => {
  await setup("setting_up");
  await expect(run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }))).rejects.toMatchObject({ message: "Cloudflare is being set up. Wait for it to finish, then switch." });
  await expect(run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "public" }))).rejects.toMatchObject({ message: "Cloudflare is being set up. Wait for it to finish, then switch." });
  expect(await attempts()).toEqual([]);
  await harness.pool.query("update organization_cloudflare_ingress set updated_at = now() - interval '11 minutes'");
  await expect(run(requestDoorsSwitch({ organizationId: ORG, userId: ADA, to: "tunnel" }))).rejects.toMatchObject({ message: "Set up Cloudflare first, then switch to it." });
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
  expect(await attempts()).toMatchObject([{ failure: "The switch stopped partway. Switch again to finish it." }]);
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
  await harness.pool.query("update organization_cloudflare_ingress set state = 'left'");
  expect(await run(freshSwitchDigest(ORG, stored, { to: "public" }))).toBeNull();
});

it("keeps a pending approval for a switch refused only for now, while Cloudflare is being set up or until it is", async () => {
  const stored = "digest";
  expect(await run(freshSwitchDigest(ORG, stored, { to: "tunnel" }))).toBe(stored);
  await setup("setting_up");
  expect(await run(freshSwitchDigest(ORG, stored, { to: "tunnel" }))).toBe(stored);
  expect(await run(freshSwitchDigest(ORG, stored, { to: "public" }))).toBe(stored);
});

it.each(["public", "tunnel"] as const)("never lets a %s switch still running at removal flip the row of the Cluster set up after it", async (to) => {
  await setup(to === "public" ? "active" : "set_up");
  await hosted();
  const request = await asked(to);
  let release!: () => void;
  let entered!: () => void;
  const reading = new Promise<void>((resolve) => { entered = resolve; });
  const gate = new Promise<void>((resolve) => { release = resolve; });
  storeRead = () => { entered(); return gate; };
  const old = run(executeDoorsOnce(request, RUN));
  await reading;
  await harness.runEffect(harness.database.transaction(endClusterIngress(ORG)));
  expect(await attempts()).toMatchObject([{ state: "failed", failure: "The Cluster was removed." }]);
    await setup("set_up");
  release();
  expect(await old).toMatchObject({ state: "failed" });
  expect(await stateNow()).toBe("set_up");
  expect(await attempts()).toMatchObject([{ state: "failed", failure: "The Cluster was removed." }]);
});
