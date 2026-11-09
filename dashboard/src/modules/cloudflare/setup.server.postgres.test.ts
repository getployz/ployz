import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect, Redacted, type Schema } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudflareHttp } from "#/modules/cloudflare/api.server";
import { checkCloudflareSetup, listCloudflareZoneNames, setUpCloudflare } from "#/modules/cloudflare/setup.server";
import { requestDoorsSwitch } from "#/modules/ingress/doors.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ORG = "00000000-0000-4000-8000-00000000c2a1";
const SLUG = "acme";
const API_TOKEN = "cf-api-token-that-must-never-be-stored";
/** A tunnel token as Cloudflare mints them: base64 JSON naming the account, the tunnel and the connector's secret. */
const TUNNEL_TOKEN = Buffer.from(JSON.stringify({ a: "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1", t: "bd0e0af1-d7e0-4d84-8bc3-d1ca9338a9ad", s: Buffer.alloc(64, 9).toString("base64") })).toString("base64");

let harness: PostgresTestHarness;

type Call = { readonly method: string; readonly path: string; readonly body: Schema.Json | undefined };

const PROBE = "{}";

type Options = {
  readonly dnsStatus?: number;
  readonly existingRecord?: string;
  /** What the tunnel create answers with as its token; null leaves it out, so the token is read on request. */
  readonly tokenOnCreate?: string | null;
  readonly recordsStatus?: number;
  readonly deleteStatus?: number;
  /** The account already holds the tunnel setup would name. */
  readonly existingTunnel?: boolean;
  /** What Cloudflare answers when the record this setup wrote is deleted; 200 when unset. */
  readonly recordDeleteStatus?: number;
  /** Runs as the record is created: the moment something else can move while setup is between its Cloudflare calls. */
  readonly onRecord?: () => Promise<void>;
  /** Runs as the tunnel this setup made is deleted: the moment another setup could reuse it. */
  readonly onTunnelDelete?: () => Promise<void>;
};

/** A Cloudflare with one zone, remembering the tunnels and records written to it. */
function cloudflare(options: Options = {}) {
  const calls: Call[] = [];
  const made: Schema.Json[] = [];
  const alive = new Set<string>(options.existingTunnel ? ["t1"] : []);
  const records: Schema.Json[] = [];
  const fetch = async (input: string | URL | Request, init?: RequestInit) => {
    const url = new URL(String(input));
    const path = `${url.pathname.replace("/client/v4", "")}${url.search}`;
    const method = init?.method ?? "GET";
    const body: Schema.Json | undefined = init?.body == null ? undefined : JSON.parse(String(init.body));
    calls.push({ method, path, body });
    const reply = (status: number, result?: Schema.Json) => Promise.resolve(new Response(JSON.stringify({ success: status < 300, errors: status < 300 ? [] : [{ code: 1, message: "no" }], result }), { status }));
    if (path.startsWith("/zones?")) return reply(200, [{ id: "z1", name: "example.com", account: { id: "a1" } }]);
    const probe = JSON.stringify(body) === PROBE;
    if (method === "POST" && path === "/zones/z1/dns_records") {
      if (probe) return reply(options.dnsStatus ?? 400);
      await options.onRecord?.();
      records.push(body ?? null);
      return reply(200, { id: "r-made" });
    }
    if (method === "DELETE" && path === "/zones/z1/dns_records/r-made") {
      if (options.recordDeleteStatus !== undefined) return reply(options.recordDeleteStatus);
      records.length = 0;
      return reply(200, {});
    }
    if (method === "GET" && path.startsWith("/zones/z1/dns_records?")) {
      if (options.recordsStatus !== undefined) return reply(options.recordsStatus);
      return reply(200, options.existingRecord === undefined ? [] : [{ id: "r1", content: options.existingRecord }]);
    }
    if (method === "POST" && path === "/accounts/a1/cfd_tunnel") {
      if (probe) return reply(400);
      made.push(body ?? null);
      alive.add("t1");
      const token = options.tokenOnCreate === undefined ? TUNNEL_TOKEN : options.tokenOnCreate;
      return reply(200, token === null ? { id: "t1" } : { id: "t1", token });
    }
    if (method === "GET" && path === "/accounts/a1/cfd_tunnel/t1/token") return reply(200, options.tokenOnCreate ?? TUNNEL_TOKEN);
    if (method === "GET" && path.startsWith("/accounts/a1/cfd_tunnel?")) return reply(200, alive.has("t1") ? [{ id: "t1" }] : []);
    if (method === "DELETE" && path === "/accounts/a1/cfd_tunnel/t1/connections") return reply(200, {});
    if (method === "DELETE" && path === "/accounts/a1/cfd_tunnel/t1") {
      await options.onTunnelDelete?.();
      if (options.deleteStatus !== undefined) return reply(options.deleteStatus);
      alive.delete("t1");
      return reply(200, {});
    }
    return reply(404);
  };
  return { http: new CloudflareHttp({ fetch }), calls, made, alive, records };
}

/** The calls that wrote something, leaving out the permission probes (an empty body) and the reads. */
const writesOf = (calls: readonly Call[]) => calls.filter(({ method, body }) => method !== "GET" && JSON.stringify(body) !== PROBE);

type Set = { readonly doors: readonly string[]; readonly token: string | undefined };

const frameOf = asTestDouble<RuntimeWatchView>();
const NO_SERVER_FRAME = frameOf({ machines: [], services: [], containers: [], incomplete_ids: { machines: [], containers: [], certificates: [] }, unanswered: [] });
/** One Server taking web traffic and running no door Container, as a Cluster founded closed is before its tunnel opens. */
const CLOSED_FRAME = frameOf({ ...NO_SERVER_FRAME, machines: [{ machine: { id: "m1", name: "s1", accepts_ingress: true, runtime: { running_builds: 0 } }, membership: "up" }] });

/** A Cluster whose Servers take any doors they are told, unless `take` says otherwise, showing `frame` when watched. */
function cluster(take: (doors: readonly string[]) => readonly string[] = (doors) => doors, frame: RuntimeWatchView = NO_SERVER_FRAME) {
  const sets: Set[] = [];
  const session: ScopedRuntimeClientSession = {
    status: "connected",
    connected: asTestDouble<PloyzSession>()({
      watchFirstFrame: () => Effect.succeed(frame),
      setIngressDoors: (doors: readonly string[], token?: Redacted.Redacted<string>) => {
        sets.push({ doors, token: token === undefined ? undefined : Redacted.value(token) });
        return Effect.succeed({ machines: [{ server: "s1", doors: take(doors) }], failures: [] });
      },
    }),
  };
  const runtime = asTestDouble<OrganizationRuntime["Service"]>()({ cancel: () => Effect.void, open: () => Effect.succeed(session) });
  return { sets, runtime };
}

/** The first Server joined on an instruction founding the Cluster with every Door closed, as the card asks for Cloudflare. */
const foundedClosed = () => harness.pool.query(`
  with person as (
    insert into "user" (id, name, email) values (gen_random_uuid(), 'Ada', gen_random_uuid() || '@example.com') returning id
  ), fleet as (
    insert into fleet_token (organization_id, name, token_hash, expires_at, founds_closed, created_by_user_id)
    select $1, 'fleet', 'hash', now() + interval '1 day', true, id from person returning id
  ), server as (
    insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip)
    select $1, id, 'key', '{}'::jsonb, '203.0.113.9' from fleet returning id, fleet_token_id
  )
  insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state)
  select $1, id, fleet_token_id, 'founder', '{"ingressClosed": true}'::jsonb, gen_random_uuid(), 'joined' from server
`, [ORG]);

const setUp = (zone: string, http: CloudflareHttp, runtime: OrganizationRuntime["Service"]) =>
  harness.runEffect(setUpCloudflare(ORG, { token: Redacted.make(API_TOKEN), zone }, http).pipe(
    Effect.provideService(OrganizationRuntime, runtime),
  ));

const setupRows = async () => (await harness.pool.query("select zone, tunnel_id, tunnel_name, state from organization_cloudflare_ingress")).rows;
const switching = () => harness.pool.query("insert into ingress_doors_attempt (id, organization_id, want, state) values (gen_random_uuid(), $1, 'tunnel', 'pending')", [ORG]);
const priorRow = (state: "set_up" | "left" | "setting_up", tunnelId: string | null = "t0") =>
  harness.pool.query("insert into organization_cloudflare_ingress (organization_id, zone, tunnel_id, tunnel_name, state) values ($1, 'example.com', $2, 'ployz-acme', $3)", [ORG, tunnelId, state]);

const inngest = new Inngest({ id: "setup-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

/** Ask for a switch to Cloudflare as the page does, and answer with what it said: joined, or why it was refused. */
const askSwitch = () => harness.runEffect(requestDoorsSwitch({ organizationId: ORG, userId: null, to: "tunnel" }).pipe(
  Effect.provideService(InngestClient, inngest),
)).then(() => "started", (error: { message: string }) => error.message);

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

beforeEach(async () => {
  await harness.pool.query(`
    truncate table operation_outbox, organization cascade;
    insert into organization (id, name, slug) values ('${ORG}', 'Acme', '${SLUG}');
  `);
});

/** Every table's rows as text, so a secret can be searched for anywhere the database holds it. */
const everything = async () => {
  const { rows } = await harness.pool.query<{ table_name: string }>("select table_name from information_schema.tables where table_schema = 'public' and table_type = 'BASE TABLE'");
  const dumps = await Promise.all(rows.map(async ({ table_name }) => (await harness.pool.query(`select t::text as row from "${table_name}" t`)).rows.map((row) => String(row.row)).join("\n")));
  return dumps.join("\n");
};

it("sets up a zone with one wildcard record, a tunnel and both doors, and moves nothing", async () => {
  const { http, calls, records, made, alive } = cloudflare();
  const { runtime, sets } = cluster();
  expect(await setUp("example.com", http, runtime)).toMatchObject({ zone: "example.com", tunnelId: "t1", state: "set_up" });

  expect(records).toEqual([{ type: "CNAME", name: "*.example.com", content: "t1.cfargotunnel.com", proxied: true, ttl: 1 }]);
  expect(made).toEqual([{ name: "ployz-acme", config_src: "local" }]);
  expect(alive.has("t1")).toBe(true);
  expect(sets).toEqual([{ doors: ["public", "tunnel"], token: TUNNEL_TOKEN }]);
  expect(await setupRows()).toEqual([{ zone: "example.com", tunnel_id: "t1", tunnel_name: "ployz-acme", state: "set_up" }]);
  expect(writesOf(calls).map(({ method, path }) => `${method} ${path}`)).toEqual(["POST /accounts/a1/cfd_tunnel", "POST /zones/z1/dns_records"]);
  expect(calls.at(-1)).toMatchObject({ method: "POST", path: "/zones/z1/dns_records" });
});

it("opens only the tunnel on a Cluster founded closed and makes the zone the source at once, so ports 80 and 443 never open", async () => {
  await foundedClosed();
  const { http, records } = cloudflare();
  const { runtime, sets } = cluster((doors) => doors, CLOSED_FRAME);
  expect(await setUp("example.com", http, runtime)).toMatchObject({ zone: "example.com", tunnelId: "t1", state: "active" });
  expect(sets).toEqual([{ doors: ["tunnel"], token: TUNNEL_TOKEN }]);
  expect(records).toHaveLength(1);
  expect(await setupRows()).toEqual([{ zone: "example.com", tunnel_id: "t1", tunnel_name: "ployz-acme", state: "active" }]);
});

it("falls back to the ports on a Cluster founded closed when its servers don't take the tunnel, and says so", async () => {
  await foundedClosed();
  const { http, alive } = cloudflare();
  const { runtime, sets } = cluster((doors) => doors.filter((door) => door !== "tunnel"), CLOSED_FRAME);
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringContaining("ports 80 and 443") });
  expect(sets.map(({ doors }) => doors)).toEqual([["tunnel"], ["public"]]);
  expect(alive.has("t1")).toBe(false);
  expect(await setupRows()).toEqual([]);
});

it("refuses on a Cluster founded closed while a Server isn't seen, opening no Door, since that Server may hold the ports", async () => {
  await foundedClosed();
  const { http, calls } = cloudflare();
  const { runtime, sets } = cluster((doors) => doors, frameOf({ ...CLOSED_FRAME, incomplete_ids: { machines: [], containers: ["m2"], certificates: [] } }));
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringMatching(/^Your servers aren't answering/) });
  expect(sets).toEqual([]);
  expect(writesOf(calls)).toEqual([]);
  expect(await setupRows()).toEqual([]);
});

it("keeps both doors on a Cluster whose ports are open, on a Server that no longer takes web traffic or one founded public", async () => {
  const publicElsewhere = frameOf({
    ...CLOSED_FRAME,
    machines: [...CLOSED_FRAME.machines, { machine: { id: "m2", name: "s2", accepts_ingress: false, runtime: { running_builds: 0 } }, membership: "up" }],
    services: [{ identity: "ployz-system/ingress-holder", containers: [{ machine_id: "m2", runtime: { state: "running" }, resolved_spec: { ports: [{ mode: "host", published_port: 80 }] } }] }],
  });
  const founded = cluster((doors) => doors, CLOSED_FRAME);
  expect(await setUp("example.com", cloudflare().http, founded.runtime)).toMatchObject({ state: "set_up" });
  expect(founded.sets.map(({ doors }) => doors)).toEqual([["public", "tunnel"]]);

  await harness.pool.query("delete from organization_cloudflare_ingress");
  await foundedClosed();
  const opened = cluster((doors) => doors, publicElsewhere);
  expect(await setUp("example.com", cloudflare().http, opened.runtime)).toMatchObject({ state: "set_up" });
  expect(opened.sets.map(({ doors }) => doors)).toEqual([["public", "tunnel"]]);
});

it("keeps the API token out of every table and the outbox, and the tunnel token out of every table", async () => {
  const { http } = cloudflare();
  const { runtime } = cluster();
  await setUp("example.com", http, runtime);
  const dump = await everything();
  expect(dump).not.toContain(API_TOKEN);
  expect(dump).not.toContain(TUNNEL_TOKEN);
  expect((await harness.pool.query("select count(*)::int as n from operation_outbox")).rows[0]).toEqual({ n: 0 });
});

it("refuses a subdomain of a zone before touching anything", async () => {
  const { http, calls } = cloudflare();
  const { runtime, sets } = cluster();
  await expect(setUp("apps.example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringContaining("Addresses go one level below the zone") });
  expect(calls.filter(({ method }) => method !== "GET")).toEqual([]);
  expect(sets).toEqual([]);
  expect(await setupRows()).toEqual([]);
});

it("names the missing permission and changes nothing when the token can't edit DNS", async () => {
  const { http, calls, alive } = cloudflare({ dnsStatus: 403 });
  const { runtime, sets } = cluster();
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringContaining("Zone: DNS: Edit") });
  expect(writesOf(calls)).toEqual([]);
  expect(alive.size).toBe(0);
  expect(sets).toEqual([]);
  expect(await setupRows()).toEqual([]);
});

it("deletes the tunnel it made, and writes no record, when the servers don't take both doors", async () => {
  const { http, records, alive } = cloudflare();
  const { runtime, sets } = cluster(() => ["public"]);
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringContaining("Nothing was changed.") });
  expect(records).toEqual([]);
  expect(alive.size).toBe(0);
  expect(sets.map(({ doors }) => doors)).toEqual([["public", "tunnel"], ["public"]]);
  expect(await setupRows()).toEqual([]);
});

it("reads the tunnel's token on request when Cloudflare sends none with the tunnel", async () => {
  const { http, calls } = cloudflare({ tokenOnCreate: null });
  const { runtime, sets } = cluster();
  expect(await setUp("example.com", http, runtime)).toMatchObject({ state: "set_up" });
  expect(sets).toEqual([{ doors: ["public", "tunnel"], token: TUNNEL_TOKEN }]);
  expect(calls.map(({ method, path }) => `${method} ${path}`)).toContain("GET /accounts/a1/cfd_tunnel/t1/token");
});

it("deletes the tunnel and asks no Server to run it when its token isn't one a connector can start on", async () => {
  const { http, alive, records } = cloudflare({ tokenOnCreate: Buffer.from(JSON.stringify({ not: "a token" })).toString("base64") });
  const { runtime, sets } = cluster();
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringContaining("Nothing was changed.") });
  expect(sets).toEqual([]);
  expect(alive.size).toBe(0);
  expect(records).toEqual([]);
  expect(await setupRows()).toEqual([]);
});

it("creates nothing and asks no Server when the record lookup fails", async () => {
  const { http, calls, alive } = cloudflare({ recordsStatus: 500 });
  const { runtime, sets } = cluster();
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringContaining("Nothing was changed.") });
  expect(sets).toEqual([]);
  expect(writesOf(calls)).toEqual([]);
  expect(alive.size).toBe(0);
  expect(await setupRows()).toEqual([]);
});

it("says the tunnel is the user's to delete when Cloudflare won't delete it after a failure", async () => {
  const { http, alive } = cloudflare({ deleteStatus: 403 });
  const { runtime } = cluster(() => ["public"]);
  const failure = await setUp("example.com", http, runtime).then(() => null, (error: { message: string }) => error.message);
  expect(failure).toContain("Delete the tunnel ployz-acme in Cloudflare.");
  expect(failure).not.toContain("Nothing was changed.");
  expect(alive.has("t1")).toBe(true);
  expect(await setupRows()).toEqual([]);
});

it("refuses while a switch is under way, before anything is created", async () => {
  await harness.pool.query("insert into organization_cloudflare_ingress (organization_id, zone, tunnel_id, tunnel_name, state) values ($1, 'example.com', 't0', 'ployz-acme', 'set_up')", [ORG]);
  await switching();
  const { http, calls } = cloudflare();
  const { runtime, sets } = cluster();
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: "A switch is under way. Wait for it to finish, then set up." });
  expect(calls).toEqual([]);
  expect(sets).toEqual([]);
  expect(await setupRows()).toMatchObject([{ tunnel_id: "t0", state: "set_up" }]);
});

it("holds the Cluster while it runs: a switch asked meanwhile is refused, and the tunnel it opened stays open", async () => {
  await priorRow("set_up");
  const answers: string[] = [];
  const { http, alive, records } = cloudflare({ onRecord: async () => {
    answers.push(await askSwitch());
    expect(await setupRows()).toEqual([{ zone: "example.com", tunnel_id: null, tunnel_name: "ployz-acme", state: "setting_up" }]);
  } });
  const { runtime, sets } = cluster();
  expect(await setUp("example.com", http, runtime)).toMatchObject({ tunnelId: "t1", state: "set_up" });
  expect(answers).toEqual(["Cloudflare is being set up. Wait for it to finish, then switch."]);
  expect(sets.map(({ doors }) => doors)).toEqual([["public", "tunnel"]]);
  expect(records).toHaveLength(1);
  expect(alive.has("t1")).toBe(true);
  expect(await setupRows()).toEqual([{ zone: "example.com", tunnel_id: "t1", tunnel_name: "ployz-acme", state: "set_up" }]);
  expect((await harness.pool.query("select count(*)::int as n from ingress_doors_attempt")).rows[0]).toEqual({ n: 0 });
});

it("refuses a second setup while one holds the Cluster, before any Cloudflare call", async () => {
  await priorRow("setting_up", null);
  const { http, calls } = cloudflare();
  const { runtime, sets } = cluster();
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: "Cloudflare is being set up. Wait for it to finish." });
  expect(calls).toEqual([]);
  expect(sets).toEqual([]);
});

it("takes over a reservation left by a setup that died", async () => {
  await priorRow("setting_up", null);
  await harness.pool.query("update organization_cloudflare_ingress set updated_at = now() - interval '11 minutes'");
  const { http } = cloudflare();
  const { runtime } = cluster();
  expect(await setUp("example.com", http, runtime)).toMatchObject({ tunnelId: "t1", state: "set_up" });
  expect(await setupRows()).toEqual([{ zone: "example.com", tunnel_id: "t1", tunnel_name: "ployz-acme", state: "set_up" }]);
});

it("holds the Cluster until its cleanup ends, so no other setup can take up the tunnel it is deleting", async () => {
  let second: string | null = null;
  const other = cloudflare({ existingTunnel: true });
  const { runtime: otherRuntime } = cluster();
  const { http } = cloudflare({
    onTunnelDelete: async () => {
      second = await setUp("example.com", other.http, otherRuntime).then(() => "set up", (error: { message: string }) => error.message);
    },
  });
  const { runtime } = cluster(() => ["public"]);
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringContaining("Nothing was changed.") });
  expect(second).toBe("Cloudflare is being set up. Wait for it to finish.");
  expect(other.calls).toEqual([]);
  expect(await setupRows()).toEqual([]);
});

it("puts the row back as it was when it fails: a Cluster that left Cloudflare is left again, with its old tunnel", async () => {
  await priorRow("left");
  const { http, alive } = cloudflare();
  const { runtime, sets } = cluster(() => ["public"]);
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: "s1 still has other doors open. Nothing was changed." });
  expect(alive.size).toBe(0);
  expect(sets.map(({ doors }) => doors)).toEqual([["public", "tunnel"], ["public"]]);
  expect(await setupRows()).toEqual([{ zone: "example.com", tunnel_id: "t0", tunnel_name: "ployz-acme", state: "left" }]);
});

it("never says nothing changed while a Server still holds the tunnel door it couldn't close", async () => {
  const { http, alive } = cloudflare();
  const { runtime, sets } = cluster((doors) => doors.length === 2 ? ["public"] : ["public", "tunnel"]);
  const failure = await setUp("example.com", http, runtime).then(() => null, (error: { message: string }) => error.message);
  expect(failure).toBe("s1 still has other doors open. Your servers still hold the tunnel door. s1 still has other doors open.");
  expect(failure).not.toContain("Nothing was changed.");
  expect(sets.map(({ doors }) => doors)).toEqual([["public", "tunnel"], ["public"]]);
  expect(alive.size).toBe(0);
  expect(await setupRows()).toEqual([]);
});

/** Another setup takes the reservation over, as one may once this one went stale: its final write must not land. */
const takenOver = () => harness.pool.query("update organization_cloudflare_ingress set updated_at = now() + interval '1 second'").then(() => undefined);

it("never lands its row over a reservation it lost: it closes the tunnel door, deletes the record it wrote, drops the tunnel's connections and deletes it", async () => {
  const { http, alive, calls, records } = cloudflare({ onRecord: takenOver });
  const { runtime, sets } = cluster();
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: "Another setup took over this one. Nothing was changed." });
  expect(records).toEqual([]);
  expect(alive.size).toBe(0);
  expect(sets.map(({ doors }) => doors)).toEqual([["public", "tunnel"], ["public"]]);
  const deletes = calls.filter(({ method }) => method === "DELETE").map(({ path }) => path);
  expect(deletes).toEqual(["/zones/z1/dns_records/r-made", "/accounts/a1/cfd_tunnel/t1/connections", "/accounts/a1/cfd_tunnel/t1"]);
  expect(await setupRows()).toMatchObject([{ state: "setting_up" }]);
});

it("names the record when Cloudflare keeps it after a failed setup, never claiming nothing changed", async () => {
  const { http, alive, records } = cloudflare({ onRecord: takenOver, recordDeleteStatus: 500 });
  const { runtime } = cluster();
  const failure = await setUp("example.com", http, runtime).then(() => null, (error: { message: string }) => error.message);
  expect(failure).toBe("Another setup took over this one. Delete *.example.com in Cloudflare.");
  expect(records).toHaveLength(1);
  expect(alive.size).toBe(0);
});

it("stops before creating anything when the zone's wildcard points elsewhere", async () => {
  const { http, calls, records, alive } = cloudflare({ existingRecord: "other.example.net" });
  const { runtime, sets } = cluster();
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: "*.example.com already points somewhere else in Cloudflare. Remove that record or pick another zone. Nothing was changed." });
  expect(writesOf(calls)).toEqual([]);
  expect(sets).toEqual([]);
  expect(records).toEqual([]);
  expect(alive.size).toBe(0);
  expect(await setupRows()).toEqual([]);
});

it("treats a wildcard pointing at a tunnel of the same name that no longer exists as pointing elsewhere", async () => {
  const { http, calls, alive } = cloudflare({ existingRecord: "t1.cfargotunnel.com" });
  const { runtime, sets } = cluster();
  await expect(setUp("example.com", http, runtime)).rejects.toMatchObject({ message: expect.stringContaining("already points somewhere else") });
  expect(writesOf(calls)).toEqual([]);
  expect(sets).toEqual([]);
  expect(alive.size).toBe(0);
});

it("accepts a wildcard that already points at the account's tunnel of that name, writing nothing more", async () => {
  const { http, records, calls } = cloudflare({ existingRecord: "t1.cfargotunnel.com", existingTunnel: true });
  const { runtime } = cluster();
  expect(await setUp("example.com", http, runtime)).toMatchObject({ state: "set_up", tunnelId: "t1" });
  expect(records).toEqual([]);
  expect(writesOf(calls)).toEqual([]);
});

it("checks a token can set a zone up before any Server exists, creating nothing", async () => {
  const { http, calls } = cloudflare();
  expect(await harness.runEffect(checkCloudflareSetup({ token: Redacted.make(API_TOKEN), zone: "example.com" }, http))).toEqual({ zone: "example.com" });
  expect(writesOf(calls)).toEqual([]);
  const taken = cloudflare({ existingRecord: "elsewhere.example.net" });
  await expect(harness.runEffect(checkCloudflareSetup({ token: Redacted.make(API_TOKEN), zone: "example.com" }, taken.http)))
    .rejects.toMatchObject({ message: expect.stringContaining("*.example.com already exists") });
  const weak = cloudflare({ dnsStatus: 403 });
  await expect(harness.runEffect(checkCloudflareSetup({ token: Redacted.make(API_TOKEN), zone: "example.com" }, weak.http)))
    .rejects.toMatchObject({ message: expect.stringContaining("DNS") });
  expect(await setupRows()).toEqual([]);
});

it("lists the zone names a token can see without changing anything", async () => {
  const { http, calls } = cloudflare();
  expect(await harness.runEffect(listCloudflareZoneNames(Redacted.make(API_TOKEN), http))).toEqual(["example.com"]);
  expect(calls).toHaveLength(1);
});
