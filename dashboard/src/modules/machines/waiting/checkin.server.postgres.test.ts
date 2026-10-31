import { generateKeyPairSync, type KeyObject, sign } from "node:crypto";
import { readFile } from "node:fs/promises";
import { Cause, Effect, Exit, Random, Schema, Tracer } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { InngestClient } from "#/modules/inngest/client";
import type { CheckinRequest, CheckinResponse, HostReport, Prep } from "#/modules/machines/waiting/checkin-wire";
import { checkIn, checkinMessage, WAITING_PER_TOKEN } from "#/modules/machines/waiting/checkin.server";
import { cancelServerEnrollment, startArrivalEnrollment, startServerEnrollment } from "#/modules/machines/waiting/enroll.server";
import { createFleetToken, listFleetTokens, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { bindServerEnrollment, closeStaleServerEnrollments, endServerEnrollment } from "#/modules/machines/waiting/enrollment-run.server";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { openProbeSelection } from "#/modules/machines/waiting/probe.server";
import { pruneStaleRateLimits } from "#/modules/machines/waiting/rate-limit.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import {
  listWaitingServers,
  showWaitingServer,
  waitingServerStatus,
} from "#/modules/machines/waiting/waiting-servers.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer, type Ployz } from "#/modules/runtime/ployz.server";
import type { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ada = { userId: "00000000-0000-4000-8000-000000000601" };
const bob = { userId: "00000000-0000-4000-8000-000000000602" };
const adaOrg = "00000000-0000-4000-8000-000000000611";

let harness: PostgresTestHarness;
const inngest = new Inngest({ id: "checkin-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

const unreachable = makePloyzLayer({ connect: async () => { throw new Error("Endpoint unavailable"); } });
const runtime = asTestDouble<OrganizationRuntime["Service"]>()({ cancel: () => Effect.void });

const run = <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | InngestClient | OrganizationRuntime | Ployz>) =>
  harness.runEffect(operation.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, runtime),
    Effect.provide(unreachable),
  ));

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

beforeEach(async () => {
  await harness.pool.query(`
    truncate table checkin_rate_limit, waiting_server, fleet_token, "user", organization cascade;
    insert into organization (id, name, slug) values
      ('${adaOrg}', 'Ada', 'ada'),
      ('00000000-0000-4000-8000-000000000612', 'Bob', 'bob');
    insert into "user" (id, email, name) values
      ('${ada.userId}', 'ada@example.test', 'Ada'),
      ('${bob.userId}', 'bob@example.test', 'Bob');
    insert into member (id, organization_id, user_id, role, created_at) values
      (gen_random_uuid(), '${adaOrg}', '${ada.userId}', 'owner', now()),
      (gen_random_uuid(), '00000000-0000-4000-8000-000000000612', '${bob.userId}', 'owner', now());
  `);
});

const report: HostReport = {
  hostname: "web-1",
  os: { id: "ubuntu", version: "24.04" },
  kernel: "6.8.0",
  arch: "x86_64",
  virt: "kvm",
  cpuCount: 4,
  memoryTotalBytes: 8_589_934_592,
  disks: [],
  docker: { kind: "absent" },
  cgroup: "v2",
  zfs: { kind: "ineligible", code: "no_disk", message: "No empty disk." },
  blockers: [],
  warnings: [],
};

const fleetToken = async (name: string) =>
  run(createFleetToken(ada, { organizationSlug: "ada", name, expiresIn: "1d", autoEnroll: null }));

type Host = { readonly key: string; readonly privateKey: KeyObject };

const newHost = (): Host => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const x = publicKey.export({ format: "jwk" }).x ?? "";
  return { key: Buffer.from(x, "base64url").toString("hex"), privateKey };
};

type Send = {
  readonly hostname?: string;
  readonly cliVersion?: string;
  readonly report?: Partial<HostReport>;
  readonly prep?: Prep;
  readonly probe?: CheckinRequest["probe"];
  readonly attempt?: CheckinRequest["attempt"];
  readonly machinePhase?: string;
  readonly skew?: number;
  readonly badSignature?: boolean;
  readonly ip?: string;
};

const request = (token: string, host: Host, options: Send = {}) => {
  const base = {
    protocolVersion: 1,
    checkinKey: host.key,
    cliVersion: options.cliVersion ?? "0.3.0",
    report: { ...report, ...options.report, hostname: options.hostname ?? report.hostname },
  } as const;
  const withPrep: CheckinRequest = options.prep === undefined ? base : { ...base, prep: options.prep };
  const withProbe: CheckinRequest = options.probe === undefined ? withPrep : { ...withPrep, probe: options.probe };
  const withAttempt: CheckinRequest = options.attempt === undefined ? withProbe : { ...withProbe, attempt: options.attempt };
  const checkin: CheckinRequest = options.machinePhase === undefined ? withAttempt : { ...withAttempt, machinePhase: options.machinePhase };
  const body = Buffer.from(JSON.stringify(checkin));
  const timestamp = BigInt(Math.floor(Date.now() / 1000) + (options.skew ?? 0));
  const signature = sign(null, checkinMessage(host.key, timestamp, body), host.privateKey);
  if (options.badSignature) signature[0] = (signature[0] ?? 0) ^ 1;
  const headers = new Headers({
    authorization: `Bearer ${token}`,
    "ployz-timestamp": String(timestamp),
    "ployz-signature": `v1=${signature.toString("base64url")}`,
  });
  return { headers, body, observedIp: options.ip ?? "203.0.113.7" };
};

const send = (token: string, host: Host, options?: Send) => run(checkIn(request(token, host, options)));

const failure = async <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | InngestClient>) => {
  const exit = await run(Effect.exit(operation));
  return Exit.isFailure(exit) ? Cause.squash(exit.cause) : null;
};

type Row = { id: string; report: HostReport & { prep?: Prep }; observed_ip: string; first_seen_at: Date; last_check_in_at: Date; rejected_at: Date | null };
const rows = async () =>
  (await harness.pool.query<Row>("select * from waiting_server order by first_seen_at, id")).rows;

const one = async () => {
  const [row, ...rest] = await rows();
  if (row === undefined || rest.length > 0) throw new Error("expected exactly one waiting_server row");
  return row;
};

const wait = { kind: "wait", retryAfter: 10 };

it("records a check-in, then updates the same row on the next one", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  expect(await send(token, host)).toEqual(wait);
  const [first] = await rows();
  expect(first).toMatchObject({ observed_ip: "203.0.113.7", rejected_at: null, report: { hostname: "web-1" } });

  expect(await send(token, host, { hostname: "web-1b", ip: "203.0.113.8" })).toEqual(wait);
  const after = await rows();
  expect(after).toHaveLength(1);
  expect(after[0]).toMatchObject({ id: first?.id, observed_ip: "203.0.113.8", report: { hostname: "web-1b" } });
  expect(after[0]?.first_seen_at).toEqual(first?.first_seen_at);
  expect(after[0]?.last_check_in_at.getTime()).toBeGreaterThan(first?.last_check_in_at.getTime() ?? Infinity);
});

it("stamps the token's last use at most once a minute", async () => {
  const { token } = await fleetToken("rack-1");
  const lastUsed = async () =>
    (await harness.pool.query<{ last_used_at: Date | null }>("select last_used_at from fleet_token")).rows[0]?.last_used_at;
  expect(await lastUsed()).toBeNull();

  const host = newHost();
  await send(token, host);
  const first = await lastUsed();
  expect(first).toBeInstanceOf(Date);
  await send(token, host);
  expect(await lastUsed()).toEqual(first);

  await harness.pool.query("update fleet_token set last_used_at = now() - interval '2 minutes'");
  const stale = await lastUsed();
  await send(token, host);
  expect((await lastUsed())?.getTime()).toBeGreaterThan(stale?.getTime() ?? Infinity);
});

it("stores prep while the host prepares, and reads a check-in without it as ready", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  await send(token, host, { prep: { docker: "installing", zfs: "installing" } });
  const preparing = await one();
  expect(preparing.report.prep).toEqual({ docker: "installing", zfs: "installing" });
  expect(waitingServerStatus(preparing.report, new Date(), new Date())).toBe("preparing");

  await send(token, host);
  const ready = await one();
  expect(ready.report.prep).toBeUndefined();
  expect(waitingServerStatus(ready.report, new Date(), new Date())).toBe("waiting");
});

const storedProbe = async (ip: string) =>
  (await harness.pool.query("select probe from waiting_server where observed_ip = $1", [ip])).rows[0]?.probe;

const settledTcp = (ip: string) => vi.waitFor(async () => {
  const tcp = (await storedProbe(ip))?.tcp;
  if (tcp === undefined || tcp["80"].seen === "pending" || tcp["443"].seen === "pending") throw new Error(`the probe for ${ip} never settled`);
  return tcp;
}, { timeout: 8_000, interval: 20 });

const spanEnded = (name: string) => {
  let resolve = () => {};
  const ended = new Promise<void>((done) => { resolve = done; });
  const tracer = Tracer.make({
    span: (options) => {
      const span = new Tracer.NativeSpan(options);
      if (options.name !== name) return span;
      const end = span.end.bind(span);
      span.end = (endTime, exit) => {
        end(endTime, exit);
        resolve();
      };
      return span;
    },
  });
  return { tracer, ended };
};

const shortenWindow = (ms: number) =>
  harness.pool.query("update waiting_server set probe = jsonb_set(probe, '{request,windowMs}', to_jsonb($1::int))", [ms]);

const handedLongAgo = () =>
  harness.pool.query("update waiting_server set probe = jsonb_set(probe, '{handedAt}', to_jsonb(now() - interval '1 hour'))");

const shownProbe = async () => {
  const [row] = await harness.runEffect(listWaitingServers(ada, { organizationSlug: "ada" }));
  return row === undefined ? undefined : (await harness.runEffect(showWaitingServer(ada, { organizationSlug: "ada", id: row.id }))).probe;
};

const waitingIds = async () => (await rows()).map((row) => row.id);

const openedNonce = async (ip: string): Promise<string> => (await storedProbe(ip))?.request.nonce;

const handedOut = (answer: CheckinResponse) => {
  if (answer.kind !== "wait" || answer.probe === undefined) throw new Error("expected a probe in the wait");
  return answer.probe.handout;
};

it("hands every selected host one shared nonce in its next wait, once", async () => {
  vi.stubEnv("PLOYZ_VERIFY_PROBE_ALLOW", "198.51.100.0/24");
  try {
    const { token } = await fleetToken("rack-1");
    const ips = ["198.51.100.11", "198.51.100.12", "198.51.100.13"];
    const hosts = ips.map(() => newHost());
    for (const [n, host] of hosts.entries()) await send(token, host, { ip: ips[n] });

    expect(await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }))).toEqual({ probing: 3 });
    const nonce = await openedNonce(ips[0] ?? "");
    await shortenWindow(1_500);

    const answers = [];
    for (const [n, host] of hosts.entries()) answers.push(await send(token, host, { ip: ips[n] }));
    for (const [n, answer] of answers.entries()) {
      expect(answer).toMatchObject({ ...wait, probe: { nonce, listen: ["udp:51820", "tcp:80", "tcp:443"] } });
      const probe = "probe" in answer ? answer.probe : undefined;
      expect(probe?.windowMs).toBeLessThanOrEqual(10_000);
      const targets = probe?.send.map((peer) => peer.target) ?? [];
      expect(targets.length).toBeLessThanOrEqual(3);
      expect(targets).not.toContain(ips[n]);
      expect(new Set(targets)).toEqual(new Set(ips.filter((ip) => ip !== ips[n])));
    }
    for (const [n, host] of hosts.entries()) expect(await send(token, host, { ip: ips[n] })).toEqual(wait);
    for (const ip of ips) await settledTcp(ip);
  } finally {
    vi.unstubAllEnvs();
  }
});

it("never selects another Organization's or a rejected host", async () => {
  const { token } = await fleetToken("rack-1");
  const [kept, rejected] = [newHost(), newHost()];
  await send(token, kept, { ip: "198.51.100.21" });
  await send(token, rejected, { ip: "198.51.100.22" });
  const ids = await waitingIds();
  expect(await harness.runEffect(openProbeSelection(bob, { organizationSlug: "bob", ids }))).toMatchObject({ probing: 0 });
  await run(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [ids[1] ?? ""] }));
  expect(await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }))).toMatchObject({ probing: 1 });
  expect(await storedProbe("198.51.100.22")).toBeNull();
});

it("records Unknown for a host at a private address without dialling it", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  await send(token, host, { ip: "10.0.0.5" });
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
  const pending = { seen: "pending" };
  expect((await storedProbe("10.0.0.5"))?.tcp).toEqual({ "80": pending, "443": pending });
  await send(token, host, { ip: "10.0.0.5" });
  const privateRange = { seen: "not_probed", reason: "private_range" };
  expect((await storedProbe("10.0.0.5"))?.tcp).toEqual({ "80": privateRange, "443": privateRange });
  expect(await shownProbe()).toEqual({
    "80": { state: "unknown", reason: "private_range" },
    "443": { state: "unknown", reason: "private_range" },
  });
});

it("answers the check-in before Cloud's connects run, and shows refusals as Closed only once the host reports it listened", async () => {
  vi.stubEnv("PLOYZ_VERIFY_PROBE_ALLOW", "127.0.0.1/32");
  try {
    const { token } = await fleetToken("rack-1");
    const host = newHost();
    await send(token, host, { ip: "127.0.0.1" });
    await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
    await shortenWindow(2_000);
    const started = performance.now();
    const handout = handedOut(await send(token, host, { ip: "127.0.0.1" }));
    expect(performance.now() - started).toBeLessThan(900);
    expect(await settledTcp("127.0.0.1")).toEqual({ "80": { seen: "refused" }, "443": { seen: "refused" } });
    expect(await shownProbe()).toEqual({ "80": { state: "unknown", reason: "pending" }, "443": { state: "unknown", reason: "pending" } });

    await send(token, host, { ip: "127.0.0.1", probe: { handout, heardFrom: [], bindFailed: ["tcp:443"] } });
    expect(await shownProbe()).toEqual({ "80": { state: "closed" }, "443": { state: "unknown", reason: "port_busy" } });
  } finally {
    vi.unstubAllEnvs();
  }
});

it("never reads Closed for a host that lost the request and never reported", async () => {
  vi.stubEnv("PLOYZ_VERIFY_PROBE_ALLOW", "127.0.0.1/32");
  try {
    const { token } = await fleetToken("rack-1");
    const host = newHost();
    await send(token, host, { ip: "127.0.0.1" });
    await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
    await shortenWindow(2_000);
    await send(token, host, { ip: "127.0.0.1" });
    await settledTcp("127.0.0.1");
    expect(await send(token, host, { ip: "127.0.0.1" })).toEqual(wait);
    await handedLongAgo();
    const noReport = { state: "unknown", reason: "no_host_report" };
    expect(await shownProbe()).toEqual({ "80": noReport, "443": noReport });
  } finally {
    vi.unstubAllEnvs();
  }
});

it("settles connects whose results were lost as no answer once the window has passed", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  await send(token, host, { ip: "198.51.100.41" });
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
  await harness.pool.query(
    "update waiting_server set probe = probe || jsonb_build_object('handedAt', now(), 'address', '198.51.100.41')",
  );
  expect(await shownProbe()).toEqual({ "80": { state: "unknown", reason: "pending" }, "443": { state: "unknown", reason: "pending" } });
  expect(await send(token, host, { ip: "198.51.100.41" })).toEqual(wait);
  await handedLongAgo();
  const noAnswer = { state: "unknown", reason: "no_answer" };
  expect(await shownProbe()).toEqual({ "80": noAnswer, "443": noAnswer });
});

it("drops results measured at an address the host has moved off, and probes it again where it is", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  await send(token, host, { ip: "127.0.0.2" });
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
  const nonce = await openedNonce("127.0.0.2");
  await harness.pool.query(`update waiting_server set probe = probe || jsonb_build_object(
    'handedAt', now(), 'address', '127.0.0.2', 'tcp', '{"80":{"seen":"answered"},"443":{"seen":"answered"}}'::jsonb)`);
  expect(await shownProbe()).toEqual({ "80": { state: "open" }, "443": { state: "open" } });

  expect(await send(token, host, { ip: "127.0.0.3" })).toMatchObject({ probe: { nonce } });
  const privateRange = { state: "unknown", reason: "private_range" };
  expect(await shownProbe()).toEqual({ "80": privateRange, "443": privateRange });
  expect(await storedProbe("127.0.0.3")).toMatchObject({ address: "127.0.0.3", request: { nonce } });
});

it("ignores a connect that finishes after the host moved to another address", async () => {
  vi.stubEnv("PLOYZ_VERIFY_PROBE_ALLOW", "127.0.0.2/32");
  try {
    const { token } = await fleetToken("rack-1");
    const host = newHost();
    await send(token, host, { ip: "127.0.0.2" });
    await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
    await shortenWindow(1_500);
    const probed = spanEnded("Probe.inbound");
    expect(await run(checkIn(request(token, host, { ip: "127.0.0.2" })).pipe(Effect.withTracer(probed.tracer)))).toHaveProperty("probe");
    expect(await send(token, host, { ip: "127.0.0.3" })).toHaveProperty("probe");
    await probed.ended;
    const privateRange = { seen: "not_probed", reason: "private_range" };
    expect((await storedProbe("127.0.0.3"))?.tcp).toEqual({ "80": privateRange, "443": privateRange });
  } finally {
    vi.unstubAllEnvs();
  }
});

it("stores a host's report only under its own handout, and keeps it across check-ins", async () => {
  const first = await fleetToken("rack-1");
  const second = await fleetToken("rack-2");
  const host = newHost();
  const ip = "198.51.100.31";
  const hostReport = (handout: string) => ({ handout, heardFrom: ["198.51.100.32"], bindFailed: ["tcp:80"] });

  expect(await send(first.token, host, { ip, probe: hostReport("old") })).toEqual(wait);
  expect(await storedProbe(ip)).toBeNull();

  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
  const nonce = await openedNonce(ip);
  const handout = handedOut(await send(first.token, host, { ip, probe: hostReport(nonce) }));
  expect(handout).not.toBe(nonce);
  expect(await storedProbe(ip)).not.toHaveProperty("host");

  expect(await send(first.token, host, { ip, probe: hostReport(handout) })).toEqual(wait);
  const reported = { heardFrom: ["198.51.100.32"], bindFailed: ["tcp:80"] };
  expect(await storedProbe(ip)).toMatchObject({ request: { nonce }, host: reported });
  await send(first.token, host, { ip });
  expect(await storedProbe(ip)).toMatchObject({ host: reported });
  await send(second.token, host, { ip });
  expect(await storedProbe(ip)).toMatchObject({ host: reported });
});

it("never counts a replayed report from the old address for the new handout", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  await send(token, host, { ip: "198.51.100.51" });
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
  const first = handedOut(await send(token, host, { ip: "198.51.100.51" }));

  const oldReport = request(token, host, { ip: "198.51.100.52", probe: { handout: first, heardFrom: [], bindFailed: [] } });
  const second = handedOut(await run(checkIn(oldReport)));
  expect(second).not.toBe(first);
  await run(checkIn(oldReport));
  expect(await storedProbe("198.51.100.52")).not.toHaveProperty("host");

  await harness.pool.query(`update waiting_server set probe = jsonb_set(probe, '{tcp}', '{"80":{"seen":"refused"},"443":{"seen":"refused"}}')`);
  await handedLongAgo();
  const noReport = { state: "unknown", reason: "no_host_report" };
  expect(await shownProbe()).toEqual({ "80": noReport, "443": noReport });
});

it("reads port busy for a host that couldn't listen, even when another host at its address answered", async () => {
  const { token } = await fleetToken("rack-1");
  const [a, b] = [newHost(), newHost()];
  const ip = "198.51.100.61";
  await send(token, a, { ip, hostname: "a" });
  await send(token, b, { ip, hostname: "b" });
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
  await send(token, a, { ip, hostname: "a" });
  const handout = handedOut(await send(token, b, { ip, hostname: "b" }));
  await harness.pool.query(`update waiting_server set probe = jsonb_set(probe, '{tcp}', '{"80":{"seen":"answered"},"443":{"seen":"other_listener"}}')`);
  await send(token, b, { ip, hostname: "b", probe: { handout, heardFrom: [], bindFailed: ["tcp:80"] } });
  const shown = await harness.runEffect(listWaitingServers(ada, { organizationSlug: "ada" }));
  const bId = shown.find((server) => server.report.hostname === "b")?.id ?? "";
  expect((await run(showWaitingServer(ada, { organizationSlug: "ada", id: bId }))).probe).toEqual({
    "80": { state: "unknown", reason: "port_busy" },
    "443": { state: "unknown", reason: "other_listener" },
  });
});

it("stops probing a host whose address keeps changing, and says so", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  await send(token, host, { ip: "198.51.100.71" });
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: await waitingIds() }));
  const handouts = [];
  for (const ip of ["198.51.100.71", "198.51.100.72", "198.51.100.73"]) handouts.push(handedOut(await send(token, host, { ip })));
  expect(new Set(handouts).size).toBe(3);

  expect(await send(token, host, { ip: "198.51.100.74" })).toEqual(wait);
  expect(await send(token, host, { ip: "198.51.100.73" })).toEqual(wait);
  const unstable = { state: "unknown", reason: "address_unstable" };
  expect(await shownProbe()).toEqual({ "80": unstable, "443": unstable });
});

it("won't reopen a selection over a host whose window is still running", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  await send(token, host, { ip: "198.51.100.81" });
  const ids = await waitingIds();
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }));
  const nonce = await openedNonce("198.51.100.81");
  await send(token, host, { ip: "198.51.100.81" });

  expect(await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }))).toEqual({ probing: 0 });
  expect(await storedProbe("198.51.100.81")).toMatchObject({ request: { nonce } });
  await handedLongAgo();
  expect(await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }))).toEqual({ probing: 1 });
  expect(await openedNonce("198.51.100.81")).not.toBe(nonce);
});

it("reopens all of a selection or none of it when one host's window is still running", async () => {
  const { token } = await fleetToken("rack-1");
  const [running, idle] = [newHost(), newHost()];
  await send(token, running, { ip: "198.51.100.82" });
  await send(token, idle, { ip: "198.51.100.83" });
  const ids = await waitingIds();
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids: [ids[0] ?? ""] }));
  const nonce = await openedNonce("198.51.100.82");
  await send(token, running, { ip: "198.51.100.82" });

  expect(await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }))).toEqual({ probing: 0 });
  expect(await openedNonce("198.51.100.82")).toBe(nonce);
  expect(await storedProbe("198.51.100.83")).toBeNull();
});

const blockedOnRowLocks = (count: number) => vi.waitFor(async () => {
  const { rows } = await harness.pool.query("select 1 from pg_stat_activity where wait_event_type = 'Lock' and datname = current_database()");
  if (rows.length < count) throw new Error(`fewer than ${count} operations waited on a row lock`);
}, { timeout: 5_000, interval: 20 });
const blockedOnRowLock = () => blockedOnRowLocks(1);

it("won't reopen over a handout a queued check-in committed while the selection waited", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  const ip = "198.51.100.84";
  await send(token, host, { ip });
  const ids = await waitingIds();
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }));
  const nonce = await openedNonce(ip);

  const checkin = await harness.pool.connect();
  try {
    await checkin.query("begin");
    await checkin.query(
      "update waiting_server set probe = probe || jsonb_build_object('handout', 'queued', 'handedAt', now(), 'address', $1::text)",
      [ip],
    );
    const reopening = harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }));
    await blockedOnRowLock();
    await checkin.query("commit");
    expect(await reopening).toEqual({ probing: 0 });
  } finally {
    checkin.release();
  }
  expect(await storedProbe(ip)).toMatchObject({ request: { nonce }, handout: "queued" });
});

it("stamps a handout when it is issued, so a check-in queued behind a long lock isn't reopened over", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  const ip = "198.51.100.85";
  await send(token, host, { ip });
  const ids = await waitingIds();
  await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }));

  const blocker = await harness.pool.connect();
  try {
    await blocker.query("begin");
    await blocker.query("select id from waiting_server for update");
    const checking = send(token, host, { ip });
    await blockedOnRowLock();
    const { rows: [queued] } = await blocker.query<{ at: string }>("select clock_timestamp()::text as at");
    const reopening = harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }));
    await blockedOnRowLocks(2);
    await blocker.query("commit");
    const handout = handedOut(await checking);
    expect(await reopening).toEqual({ probing: 0 });
    expect(await storedProbe(ip)).toMatchObject({ handout });
    const { rows: [stamped] } = await harness.pool.query<{ late: boolean }>(
      "select (probe->>'handedAt')::timestamptz > $1::timestamptz as late from waiting_server where observed_ip = $2",
      [queued?.at, ip],
    );
    expect(stamped?.late).toBe(true);
  } finally {
    blocker.release();
  }
});

const heapFirstBiggestId = async (token: string) => {
  for (const ip of ["198.51.100.86", "198.51.100.87"]) await send(token, newHost(), { ip });
  const big = "00000000-0000-4000-8000-0000000000f2";
  const small = "00000000-0000-4000-8000-0000000000f1";
  await harness.pool.query("update waiting_server set id = $1 where observed_ip = '198.51.100.86'", [big]);
  await harness.pool.query("update waiting_server set id = $1 where observed_ip = '198.51.100.87'", [small]);
  return { big, small };
};

const smallLockedWhileBigHeld = async (writer: (ids: ReadonlyArray<string>) => Promise<void>, token: string) => {
  const { big, small } = await heapFirstBiggestId(token);
  const blocker = await harness.pool.connect();
  const probe = await harness.pool.connect();
  try {
    await blocker.query("begin");
    await blocker.query("select id from waiting_server where id = $1 for update", [big]);
    const writing = writer([big, small]);
    await blockedOnRowLock();
    await probe.query("begin");
    const lockedSmall = await probe.query("select id from waiting_server where id = $1 for update nowait", [small])
      .then(() => false, () => true);
    await probe.query("rollback");
    await blocker.query("commit");
    await writing;
    return lockedSmall;
  } finally {
    blocker.release();
    probe.release();
  }
};

it("locks waiting rows in id order in every writer of several, so they can't deadlock each other", async () => {
  const writers = {
    select: async (ids: ReadonlyArray<string>) => {
      await harness.runEffect(openProbeSelection(ada, { organizationSlug: "ada", ids }));
    },
    reject: async (ids: ReadonlyArray<string>) => {
      await run(rejectWaitingServers(ada, { organizationSlug: "ada", ids }));
    },
    revoke: async () => {
      await harness.runEffect(revokeFleetToken(ada, { organizationSlug: "ada", name: "lock-order" }));
    },
  };
  for (const [name, writer] of Object.entries(writers)) {
    await harness.pool.query("truncate table waiting_server, fleet_token cascade");
    const { token } = await fleetToken("lock-order");
    expect({ name, lockedSmall: await smallLockedWhileBigHeld(writer, token) }).toEqual({ name, lockedSmall: true });
  }
}, 20_000);

it("lets two Rejects finish when a check-in un-rejects one of their rows between their lock and their write", async () => {
  const first = await fleetToken("rack-1");
  const replacement = await fleetToken("rack-2");
  const tombstone = newHost();
  await send(first.token, tombstone, { ip: "198.51.100.88" });
  await send(first.token, newHost(), { ip: "198.51.100.89" });
  const small = "00000000-0000-4000-8000-0000000000f1";
  const big = "00000000-0000-4000-8000-0000000000f2";
  await harness.pool.query("update waiting_server set id = $1 where observed_ip = '198.51.100.88'", [small]);
  await harness.pool.query("update waiting_server set id = $1 where observed_ip = '198.51.100.89'", [big]);
  const reject = () => run(Effect.exit(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [small, big] })));
  expect(await run(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [small] }))).toEqual({ rejected: 1, spared: [] });

  const blocker = await harness.pool.connect();
  try {
    await blocker.query("begin");
    await blocker.query("select id from waiting_server where id = $1 for update", [big]);
    const earlier = reject();
    await blockedOnRowLock();
    expect(await send(replacement.token, tombstone, { ip: "198.51.100.88" })).toEqual(wait);
    const later = reject();
    await blockedOnRowLocks(2);
    await blocker.query("commit");
    const outcomes = await Promise.all([earlier, later]);
    expect(outcomes.map((exit) => Exit.isSuccess(exit) ? exit.value : Cause.squash(exit.cause)))
      .toEqual([{ rejected: 1, spared: [] }, { rejected: 1, spared: [] }]);
  } finally {
    blocker.release();
  }
}, 15_000);

it("refuses an unsigned, mis-signed, stale or unknown check-in and writes nothing", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  const unauthorized = { _tag: "Unauthorized" };
  expect(await failure(checkIn(request(token, host, { badSignature: true })))).toMatchObject(unauthorized);
  expect(await failure(checkIn(request(token, host, { skew: 400 })))).toMatchObject(unauthorized);
  expect(await failure(checkIn(request(token, host, { skew: -400 })))).toMatchObject(unauthorized);
  expect(await failure(checkIn(request(`${token}x`, host)))).toMatchObject(unauthorized);
  const unsigned = request(token, host);
  unsigned.headers.delete("ployz-signature");
  expect(await failure(checkIn(unsigned))).toMatchObject(unauthorized);
  const otherKey = request(token, newHost());
  expect(await failure(checkIn({ ...request(token, host), headers: otherKey.headers }))).toMatchObject(unauthorized);
  expect(await rows()).toEqual([]);
});

it("refuses a signed body that isn't a check-in with 400, and an unsigned one with 401", async () => {
  const { token } = await fleetToken("rack-1");
  const host = newHost();
  const signed = request(token, host);
  const body = Buffer.from(JSON.stringify({ protocolVersion: 2, checkinKey: host.key }));
  const timestamp = BigInt(signed.headers.get("ployz-timestamp") ?? "");
  const signature = sign(null, checkinMessage(host.key, timestamp, body), host.privateKey).toString("base64url");
  const headers = new Headers(signed.headers);
  headers.set("ployz-signature", `v1=${signature}`);
  expect(await failure(checkIn({ ...signed, headers, body }))).toMatchObject({ _tag: "CheckinRefused", status: 400 });
  expect(await failure(checkIn({ ...signed, body: Buffer.from("not json") }))).toMatchObject({ _tag: "Unauthorized" });
  expect(await failure(checkIn({ ...signed, body }))).toMatchObject({ _tag: "Unauthorized" });
  expect(await rows()).toEqual([]);
});

it("tells hosts on an expired or revoked token to stop, and removes their rows", async () => {
  const expiring = await fleetToken("rack-1");
  const revoking = await fleetToken("rack-2");
  const [first, second] = [newHost(), newHost()];
  await send(expiring.token, first);
  await send(revoking.token, second);

  await harness.pool.query("update fleet_token set expires_at = now() - interval '1 second' where id = $1", [expiring.id]);
  expect(await send(expiring.token, first)).toEqual({ kind: "stop", reason: "token_expired", message: "The Fleet Token expired." });
  await harness.pool.query("update fleet_token set revoked_at = now() where id = $1", [revoking.id]);
  await harness.pool.query("truncate checkin_rate_limit");
  expect(await send(revoking.token, second, { ip: "198.51.100.77" })).toEqual({ kind: "stop", reason: "token_revoked", message: "The Fleet Token was revoked." });
  expect(await send(expiring.token, first, { ip: "198.51.100.78" })).toMatchObject({ kind: "stop", reason: "token_expired" });
  expect(await rows()).toEqual([]);
  expect((await harness.pool.query("select key from checkin_rate_limit")).rows).toEqual([]);
});

it("prunes rate-limit windows that have passed", async () => {
  await harness.pool.query(`
    insert into checkin_rate_limit (key, window_start, count) values
      ('ip:198.51.100.1', now() - interval '1 hour', 3),
      ('ip:198.51.100.2', date_trunc('second', now()) + interval '1 hour', 1)`);
  await run(pruneStaleRateLimits());
  expect((await harness.pool.query("select key from checkin_rate_limit")).rows).toEqual([{ key: "ip:198.51.100.2" }]);
});

it("prunes passed rate-limit windows on the check-ins its sample picks", async () => {
  const { token } = await fleetToken("rack-1");
  await harness.pool.query("insert into checkin_rate_limit (key, window_start, count) values ('ip:198.51.100.1', now() - interval '1 hour', 3)");
  const sampled = (draw: number) =>
    run(checkIn(request(token, newHost(), { ip: "198.51.100.60" })).pipe(
      Effect.provideService(Random.Random, { nextDoubleUnsafe: () => draw, nextIntUnsafe: () => 0 }),
    ));
  const stale = async () => (await harness.pool.query("select 1 from checkin_rate_limit where key = 'ip:198.51.100.1'")).rowCount;
  expect(await sampled(0.5)).toEqual(wait);
  expect(await stale()).toBe(1);
  expect(await sampled(0)).toEqual(wait);
  expect(await stale()).toBe(0);
});

it("doesn't spend the source address's budget on a check-in the token's budget refused", async () => {
  const { token, id } = await fleetToken("rack-1");
  await harness.pool.query(
    "insert into checkin_rate_limit (key, window_start, count) values ($1, date_trunc('second', now()) + interval '1 hour', 10000)",
    [`token:${id}`],
  );
  expect(await failure(checkIn(request(token, newHost(), { ip: "198.51.100.40" })))).toMatchObject({ status: 429 });
  expect((await harness.pool.query("select key from checkin_rate_limit where key like 'ip:%'")).rows).toEqual([]);
});

it("keeps a rejected host stopped under its token, and lets a new token admit it again", async () => {
  const first = await fleetToken("rack-1");
  const host = newHost();
  await send(first.token, host);
  const row = await one();
  expect(await run(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [row.id] }))).toEqual({ rejected: 1, spared: [] });
  expect(await run(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [row.id] }))).toEqual({ rejected: 0, spared: [] });
  expect(await run(listWaitingServers(ada, { organizationSlug: "ada" }))).toEqual([]);

  expect(await send(first.token, host)).toEqual({ kind: "stop", reason: "rejected", message: "This server was rejected in Ployz Cloud." });
  expect((await one()).rejected_at).toBeInstanceOf(Date);

  const second = await fleetToken("rack-2");
  expect(await send(second.token, host)).toEqual(wait);
  expect(await one()).toMatchObject({ id: row.id, rejected_at: null });
});

it(`refuses a new host once a token has ${WAITING_PER_TOKEN} waiting, but still answers the ones it has`, async () => {
  const { token, id } = await fleetToken("rack-1");
  const known = newHost();
  await send(token, known);
  await harness.pool.query(
    `insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip)
     select $1, $2, lpad(to_hex(n), 64, '0'), $3::jsonb, '203.0.113.9' from generate_series(1, $4::int) n`,
    [adaOrg, id, JSON.stringify(report), WAITING_PER_TOKEN - 1],
  );
  expect(await failure(checkIn(request(token, newHost())))).toMatchObject({ _tag: "CheckinRefused", status: 429 });
  expect(await send(token, known)).toEqual(wait);
  expect(await rows()).toHaveLength(WAITING_PER_TOKEN);
});

it("lets only as many concurrent new hosts in as the cap has room for", async () => {
  const { token, id } = await fleetToken("rack-1");
  await harness.pool.query(
    `insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip)
     select $1, $2, lpad(to_hex(n), 64, '0'), $3::jsonb, '203.0.113.9' from generate_series(1, $4::int) n`,
    [adaOrg, id, JSON.stringify(report), WAITING_PER_TOKEN - 2],
  );
  const outcomes = await Promise.all(
    Array.from({ length: 6 }, (_, n) =>
      run(Effect.exit(checkIn(request(token, newHost(), { ip: `198.51.100.${n + 1}` })))),
    ),
  );
  expect(outcomes.filter((exit) => Exit.isSuccess(exit))).toHaveLength(2);
  expect(await rows()).toHaveLength(WAITING_PER_TOKEN);
});

it.each([
  { case: "moves to another token", thenRevoke: false },
  { case: "moves to another token that is then revoked", thenRevoke: true },
])("holds a token at the cap when a host it refreshes $case mid check-in", async ({ thenRevoke }) => {
  const a = await fleetToken("rack-1");
  const b = await fleetToken("rack-2");
  const moving = newHost();
  await send(a.token, moving);
  await harness.pool.query(
    `insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip)
     select $1, $2, lpad(to_hex(n), 64, '0'), $3::jsonb, '203.0.113.9' from generate_series(1, $4::int) n`,
    [adaOrg, a.id, JSON.stringify(report), WAITING_PER_TOKEN - 1],
  );
  const held = async () =>
    Number((await harness.pool.query("select count(*) from waiting_server where fleet_token_id = $1", [a.id])).rows[0]?.count);

  const holder = await harness.pool.connect();
  try {
    await holder.query("begin");
    await holder.query("select 1 from fleet_token where id = $1 for update", [a.id]);
    const refresh = run(Effect.exit(checkIn(request(a.token, moving, { ip: "198.51.100.50" }))));
    await blockedOnRowLock();
    expect(await send(b.token, moving, { ip: "198.51.100.51" })).toEqual(wait);
    if (thenRevoke) {
      expect(await run(revokeFleetToken(ada, { organizationSlug: "ada", id: b.id }))).toEqual({ waitingRemoved: 1 });
    }
    await holder.query(
      `insert into waiting_server (organization_id, fleet_token_id, checkin_key, report, observed_ip)
       values ($1, $2, repeat('f', 64), $3::jsonb, '203.0.113.9')`,
      [adaOrg, a.id, JSON.stringify(report)],
    );
    await holder.query("commit");
    const outcome = await refresh;
    expect(Exit.isFailure(outcome) && Cause.squash(outcome.cause)).toMatchObject({ _tag: "CheckinRefused", status: 429 });
  } finally {
    holder.release();
  }
  expect(await held()).toBe(WAITING_PER_TOKEN);
});

it("rate limits one source address and keeps what it already wrote", async () => {
  const { token } = await fleetToken("rack-1");
  // A window that ends after the test keeps every check-in in one count, however slowly they run.
  await harness.pool.query(
    "insert into checkin_rate_limit (key, window_start, count) values ('ip:198.51.100.1', now() + interval '1 hour', 0)",
  );
  const hosts = Array.from({ length: 120 }, newHost);
  const outcomes = [];
  for (const host of hosts) outcomes.push(await run(Effect.exit(checkIn(request(token, host, { ip: "198.51.100.1" })))));
  const refused = outcomes.filter((exit) => Exit.isFailure(exit));
  expect(refused.length).toBeGreaterThan(0);
  for (const exit of refused) expect(Exit.isFailure(exit) && Cause.squash(exit.cause)).toMatchObject({ status: 429 });
  expect(await rows()).toHaveLength(outcomes.length - refused.length);
  expect(await send(token, newHost(), { ip: "198.51.100.2" })).toEqual(wait);
});

it("removes a revoked token's waiting hosts and counts them", async () => {
  const { token, id } = await fleetToken("rack-1");
  const other = await fleetToken("rack-2");
  for (const host of [newHost(), newHost(), newHost()]) await send(token, host);
  await send(other.token, newHost());
  expect(await run(revokeFleetToken(ada, { organizationSlug: "ada", id }))).toEqual({ waitingRemoved: 3 });
  expect(await rows()).toHaveLength(1);
});

it("lists and shows waiting hosts only to their own Organization", async () => {
  const { token } = await fleetToken("rack-1");
  await send(token, newHost(), { hostname: "<script>alert(1)</script>" });
  const [listed] = await run(listWaitingServers(ada, { organizationSlug: "ada" }));
  const id = listed?.id ?? "";
  expect(listed).toMatchObject({ fleetToken: "rack-1", status: "waiting", report: { hostname: "<script>alert(1)</script>" } });
  const detail = await run(showWaitingServer(ada, { organizationSlug: "ada", id }));
  expect(detail).toMatchObject({ id, report: { hostname: "<script>alert(1)</script>" }, probe: null });
  expect(detail.report).toHaveProperty("kernel");
  expect(listed).not.toHaveProperty("probe");
  expect(listed?.report).not.toHaveProperty("kernel");
  expect(listed?.report).not.toHaveProperty("warnings");
  expect(await failure(showWaitingServer(bob, { organizationSlug: "bob", id })))
    .toMatchObject({ _tag: "NotFound", message: "No such waiting server." });
  expect(await run(rejectWaitingServers(bob, { organizationSlug: "bob", ids: [id] }))).toEqual({ rejected: 0, spared: [] });
});

it("drops a host from the list once it is enrolling or joined, and brings it back when an attempt fails", async () => {
  const { token, id: tokenId } = await fleetToken("rack-1");
  await send(token, newHost());
  const [listed] = await run(listWaitingServers(ada, { organizationSlug: "ada" }));
  const enroll = (state: string, ended: boolean) => harness.pool.query(
    `insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state, ended_at)
     values ($1, $2, $3, 'joiner', '{}', gen_random_uuid(), $4, ${ended ? "now()" : "null"})`,
    [adaOrg, listed?.id, tokenId, state],
  );
  const shown = async () => (await run(listWaitingServers(ada, { organizationSlug: "ada" }))).map((server) => server.id);

  await enroll("running", false);
  expect(await shown()).toEqual([]);
  await harness.pool.query("update server_enrollment set state = 'failed', ended_at = now()");
  expect(await shown()).toEqual([listed?.id]);
  await enroll("joined", true);
  expect(await shown()).toEqual([]);
});

it("leaves an ended token's last use alone", async () => {
  const { token, id } = await fleetToken("rack-1");
  await harness.pool.query("update fleet_token set expires_at = now() - interval '1 second' where id = $1", [id]);
  expect(await send(token, newHost())).toMatchObject({ kind: "stop", reason: "token_expired" });
  expect((await harness.pool.query("select last_used_at from fleet_token")).rows).toEqual([{ last_used_at: null }]);
});

it("leaves an ended token's last use alone while one of its hosts is still enrolling", async () => {
  const { token, id } = await fleetToken("rack-1");
  const host = newHost();
  await send(token, host);
  const [listed] = await run(listWaitingServers(ada, { organizationSlug: "ada" }));
  await harness.pool.query(
    `insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state)
     values ($1, $2, $3, 'joiner', '{}', gen_random_uuid(), 'running')`,
    [adaOrg, listed?.id, id],
  );
  await harness.pool.query("update fleet_token set last_used_at = null, expires_at = now() - interval '1 second' where id = $1", [id]);

  expect(await send(token, host)).not.toMatchObject({ kind: "stop" });
  expect((await harness.pool.query("select last_used_at from fleet_token")).rows).toEqual([{ last_used_at: null }]);
});

it("reads status from the last check-in and the report", () => {
  const now = new Date("2026-10-16T00:01:00Z");
  const recent = new Date("2026-10-16T00:00:50Z");
  expect(waitingServerStatus(report, new Date("2026-10-16T00:00:29Z"), now)).toBe("offline");
  expect(waitingServerStatus({ ...report, blockers: [{ code: "cgroup_v1", message: "cgroup v1." }] }, recent, now)).toBe("blocked");
  expect(waitingServerStatus({ ...report, prep: { docker: "failed", zfs: "off" } }, recent, now)).toBe("preparing");
  expect(waitingServerStatus({ ...report, prep: { docker: "ready", zfs: "installing" } }, recent, now)).toBe("waiting");
});

const readyAuto = { cliVersion: "0.3.1", prep: { docker: "ready", zfs: "ready" } } as const;
const arrivalWait = { kind: "wait", retryAfter: 2 };
const noCluster = { ...wait, noCluster: true };
const autoToken = (maxServers = 2) => run(createFleetToken(ada, { organizationSlug: "ada", name: "auto", expiresIn: "30d", autoEnroll: { maxServers } }));
const foundOrganization = () => harness.pool.query(`insert into organization_pairing (organization_id, founder_machine_id, founder_claim_machine_id, founder_public_key, encrypted_pairing_secret)
  values ($1, repeat('a', 32), repeat('a', 32), repeat('b', 64), '{}')`, [adaOrg]);
const autoAttempts = async () => (await harness.pool.query<{ id: string; waiting_server_id: string; role: string; state: string; instruction: unknown }>("select * from server_enrollment order by requested_at, id")).rows;
const endAuto = async (id: string, state: "joined" | "failed" | "cancelled") => {
  const runId = `auto-${id}`;
  await run(bindServerEnrollment({ attemptId: id, organizationId: adaOrg }, runId));
  const ending = state === "joined"
    ? { state, machineId: Schema.decodeUnknownSync(rustMachineIdSchema)("a".repeat(32)) }
    : state === "failed" ? { state, failure: { stage: null, message: "Install failed" } } : { state };
  expect(await run(endServerEnrollment(id, runId, ending))).toBe(true);
};

it("holds an arrival token's maximum under concurrent signed check-ins", async () => {
  await foundOrganization();
  const { token } = await autoToken(5);
  const answers = await Promise.all(Array.from({ length: 20 }, (_, n) => send(token, newHost(), { ...readyAuto, hostname: `auto-${n}`, ip: `198.51.100.${n + 1}` })));
  expect(answers.filter((answer) => answer.kind === "wait" && answer.retryAfter === 2)).toHaveLength(5);
  expect(answers.filter((answer) => answer.kind === "wait" && answer.retryAfter === 10)).toHaveLength(15);
  const attempts = await autoAttempts();
  expect(attempts).toHaveLength(5);
  expect(attempts.every((attempt) => attempt.role === "joiner" && attempt.state === "instructed")).toBe(true);
  expect((await harness.pool.query("select count(*)::int as count from operation_outbox where name = 'server/enrollment.requested'")).rows).toEqual([{ count: 5 }]);
});

it("revokes an arrival token mid-burst without deadlock and keeps every started attempt", async () => {
  await foundOrganization();
  const { token, id } = await autoToken(10);
  const arrivals = Array.from({ length: 12 }, (_, n) => send(token, newHost(), { ...readyAuto, hostname: `burst-${n}`, ip: `198.51.100.${n + 1}` }));
  const revoked = run(revokeFleetToken(ada, { organizationSlug: "ada", id }));
  const answers = await Promise.all(arrivals);
  await revoked;
  const started = answers.filter((answer) => answer.kind === "wait" && answer.retryAfter === 2).length;
  expect(await autoAttempts()).toHaveLength(started);
  expect(await rows()).toHaveLength(started);
});

it("never founds on arrival, including while the first server is still founding", async () => {
  const { token } = await autoToken();
  const host = newHost();
  expect(await send(token, host, readyAuto)).toEqual(noCluster);
  expect(await autoAttempts()).toHaveLength(0);
  await foundOrganization();
  await harness.pool.query("update organization_pairing set founder_machine_id = null");
  expect(await send(token, host, readyAuto)).toEqual(noCluster);
  expect(await autoAttempts()).toHaveLength(0);
  await harness.pool.query("update organization_pairing set founder_machine_id = repeat('a', 32)");
  expect(await send(token, host, readyAuto)).toEqual(arrivalWait);
  expect(await send(token, host, readyAuto)).toMatchObject({ kind: "enroll", role: "joiner", name: "web-1" });
  expect(await autoAttempts()).toHaveLength(1);
});

it("says an arrival host waits on a first server only while the Organization has none", async () => {
  const plain = await fleetToken("review");
  const { token } = await autoToken();
  const host = newHost();
  expect(await send(plain.token, newHost(), { ...readyAuto, hostname: "first" })).toEqual(wait);
  expect(await send(token, host, readyAuto)).toEqual(noCluster);
  const first = (await rows()).find((row) => row.report.hostname === "first")?.id ?? "";
  await run(startServerEnrollment(ada, { organizationSlug: "ada", ids: [first], names: { kind: "hostnames" }, storage: { kind: "none" }, runs: { services: true, ingress: true, builds: true } }));
  expect(await autoAttempts()).toMatchObject([{ role: "founder" }]);
  expect(await send(token, host, readyAuto)).toEqual(wait);
});

it("keeps plain tokens and older clients waiting in a founded Organization", async () => {
  await foundOrganization();
  const plain = await fleetToken("review");
  const auto = await autoToken();
  expect(await send(plain.token, newHost(), readyAuto)).toEqual(wait);
  expect(await send(auto.token, newHost(), { ...readyAuto, cliVersion: "0.2.9" })).toEqual(wait);
  expect(await autoAttempts()).toHaveLength(0);
});

it.each([
  { label: "blocker", options: { ...readyAuto, report: { blockers: [{ code: "disk_too_small", message: "Disk too small" }] } } },
  { label: "Docker installing", options: { ...readyAuto, prep: { docker: "installing", zfs: "ready" } } },
  { label: "Docker failed", options: { ...readyAuto, prep: { docker: "failed", zfs: "ready" } } },
  { label: "ZFS installing", options: { ...readyAuto, prep: { docker: "ready", zfs: "installing" } } },
] satisfies ReadonlyArray<{ label: string; options: Send }>)
("waits for $label before using an arrival slot, then plans the finished build", async ({ options }) => {
  await foundOrganization();
  const { token } = await autoToken();
  const host = newHost();
  expect(await send(token, host, options)).toEqual(wait);
  expect(await autoAttempts()).toHaveLength(0);
  expect(await send(token, host, { ...readyAuto, report: { zfs: { kind: "eligible", targets: [{ kind: "disk", byId: "spare-auto" }] } } })).toEqual(arrivalWait);
  expect(await send(token, host, readyAuto)).toMatchObject({ kind: "enroll", storage: { kind: "zfs", device: "/dev/disk/by-id/spare-auto" } });
});

it.each(["failed", "skipped", "off"] as const)("uses Docker volumes after ZFS preparation is %s", async (zfs) => {
  await foundOrganization();
  const { token } = await autoToken();
  const host = newHost();
  expect(await send(token, host, { ...readyAuto, prep: { docker: "ready", zfs }, report: { zfs: { kind: "eligible", targets: [{ kind: "main_disk" }] } } })).toEqual(arrivalWait);
  expect(await send(token, host, readyAuto)).toMatchObject({ kind: "enroll", storage: { kind: "none" } });
});

it.each(["failed", "cancelled"] as const)("frees an arrival slot when an attempt is %s", async (state) => {
  await foundOrganization();
  const { token } = await autoToken(1);
  const first = newHost();
  const next = newHost();
  expect(await send(token, first, readyAuto)).toEqual(arrivalWait);
  expect(await send(token, next, readyAuto)).toEqual(wait);
  const attempt = (await autoAttempts())[0];
  expect(attempt).toBeDefined();
  await endAuto(attempt?.id ?? "", state);
  expect(await send(token, next, readyAuto)).toEqual(arrivalWait);
  expect(await send(token, first, readyAuto)).toEqual(wait);
});

it.each(["failed", "cancelled"] as const)("keeps a host whose arrival attempt %s in Waiting until someone enrolls it again", async (state) => {
  await foundOrganization();
  const { token } = await autoToken(5);
  const host = newHost();
  expect(await send(token, host, readyAuto)).toEqual(arrivalWait);
  const attempt = (await autoAttempts())[0]?.id ?? "";
  if (state === "failed") await endAuto(attempt, state);
  else expect(await run(cancelServerEnrollment(ada, { organizationSlug: "ada", id: attempt }))).toEqual({ id: attempt, cancelling: true });
  expect(await send(token, host, readyAuto)).toEqual(wait);
  expect(await send(token, host, readyAuto)).toEqual(wait);
  expect(await autoAttempts()).toMatchObject([{ state }]);
  const input = { ids: [(await one()).id], names: { kind: "hostnames" }, storage: { kind: "none" }, runs: { services: true, ingress: true, builds: true } } as const;
  await run(startServerEnrollment(ada, { organizationSlug: "ada", ...input }));
  expect(await send(token, host, readyAuto)).toMatchObject({ kind: "enroll", role: "joiner" });
  expect(await autoAttempts()).toMatchObject([{ state }, { state: "instructed" }]);
});

it.each(["revoked", "expired"] as const)("keeps a host's spent arrival after it moves to a token that is then %s", async (end) => {
  await foundOrganization();
  const { token } = await autoToken(5);
  const other = await run(createFleetToken(ada, { organizationSlug: "ada", name: "other", expiresIn: "30d", autoEnroll: null }));
  const host = newHost();
  expect(await send(token, host, readyAuto)).toEqual(arrivalWait);
  await endAuto((await autoAttempts())[0]?.id ?? "", "failed");
  expect(await send(other.token, host, readyAuto)).toEqual(wait);
  if (end === "revoked") await run(revokeFleetToken(ada, { organizationSlug: "ada", id: other.id }));
  else {
    await harness.pool.query("update fleet_token set expires_at = now() - interval '1 second' where id = $1", [other.id]);
    expect(await send(other.token, host, readyAuto)).toMatchObject({ kind: "stop", reason: "token_expired" });
  }
  expect(await rows()).toHaveLength(0);
  expect(await send(token, host, readyAuto)).toEqual(wait);
  expect(await autoAttempts()).toHaveLength(0);
});

it("refuses a second arrival to a joined host that returns on its token after the seven-day sweep", async () => {
  await foundOrganization();
  const { token } = await autoToken(5);
  const host = newHost();
  expect(await send(token, host, readyAuto)).toEqual(arrivalWait);
  await endAuto((await autoAttempts())[0]?.id ?? "", "joined");
  await harness.pool.query("update server_enrollment set ended_at = now() - interval '8 days'");
  expect(await run(closeStaleServerEnrollments())).toEqual({ failed: 0, removed: 1 });
  expect(await send(token, host, readyAuto)).toEqual(wait);
  expect(await autoAttempts()).toHaveLength(0);
});

it("brings a joined host back to Waiting once its Server is gone, without a second arrival", async () => {
  await foundOrganization();
  const { token } = await autoToken(5);
  const host = newHost();
  expect(await send(token, host, readyAuto)).toEqual(arrivalWait);
  const attempt = (await autoAttempts())[0]?.id ?? "";
  await endAuto(attempt, "joined");
  await harness.pool.query(`insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability)
    values ($1, repeat('a', 32), repeat('c', 64), '{}')`, [adaOrg]);
  // A reset host still names the attempt that joined it.
  const reset = { ...readyAuto, attempt: { attemptId: attempt, stage: "publishing" } };
  const shown = async () => (await run(listWaitingServers(ada, { organizationSlug: "ada" }))).map((server) => server.report.hostname);

  expect(await send(token, host, reset)).toEqual(wait);
  expect(await shown()).toEqual([]);
  await harness.pool.query("delete from organization_machine");
  expect(await send(token, host, reset)).toEqual(wait);
  expect(await send(token, host, reset)).toEqual(wait);
  expect(await shown()).toEqual(["web-1"]);
  expect(await autoAttempts()).toHaveLength(0);
  expect((await run(listFleetTokens(ada, { organizationSlug: "ada" })))[0]).toMatchObject({ joined: 1, waiting: 1 });

  const input = { ids: [(await one()).id], names: { kind: "hostnames" }, storage: { kind: "none" }, runs: { services: true, ingress: true, builds: true } } as const;
  await run(startServerEnrollment(ada, { organizationSlug: "ada", ...input }));
  expect(await send(token, host, reset)).toMatchObject({ kind: "enroll", role: "joiner" });
});

it("shows a host whose ployzd holds no Machine, checking in with a joined Server's key, as Blocked while that Server is listed", async () => {
  await foundOrganization();
  const { token } = await autoToken(5);
  const host = newHost();
  expect(await send(token, host, readyAuto)).toEqual(arrivalWait);
  const attempt = (await autoAttempts())[0]?.id ?? "";
  await endAuto(attempt, "joined");
  await harness.pool.query(`insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, machine_name)
    values ($1, repeat('a', 32), repeat('c', 64), '{}', 'web-old')`, [adaOrg]);
  // Uninstalled by a ployz-uninstall that kept its check-in key, and installed again.
  const reset = { ...readyAuto, attempt: { attemptId: attempt, stage: "publishing" }, machinePhase: "uninitialized" };
  const listed = () => run(listWaitingServers(ada, { organizationSlug: "ada" }));

  expect(await send(token, host, reset)).toEqual(wait);
  expect(await listed()).toMatchObject([{
    status: "blocked",
    report: {
      hostname: "web-1",
      blockers: [{ code: "former_server", message: "This server was web-old, which is still listed. Forget web-old to add it again: ployz server forget web-old" }],
    },
  }]);
  expect(await autoAttempts()).toHaveLength(1);

  await harness.pool.query("delete from organization_machine");
  expect(await send(token, host, reset)).toEqual(wait);
  expect(await send(token, host, reset)).toEqual(wait);
  expect(await listed()).toMatchObject([{ status: "waiting", report: { hostname: "web-1", blockers: [] } }]);
});

it("keeps a joined Server hidden when its last check-in was the one before it joined", async () => {
  await foundOrganization();
  const { token } = await autoToken(5);
  // Its ployzd held no Machine yet, and no check-in got through after the join.
  expect(await send(token, newHost(), { ...readyAuto, machinePhase: "uninitialized" })).toEqual(arrivalWait);
  await endAuto((await autoAttempts())[0]?.id ?? "", "joined");
  await harness.pool.query(`insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, machine_name)
    values ($1, repeat('a', 32), repeat('c', 64), '{}', 'web-old')`, [adaOrg]);

  expect(await run(listWaitingServers(ada, { organizationSlug: "ada" }))).toEqual([]);
});

it.each([
  ["re-runs its install", "participating", undefined],
  ["missed Cloud's answer to its join", "participating", { stage: "publishing", message: "Cloud did not answer" }],
  ["has a ployzd that doesn't answer, or an older CLI", undefined, undefined],
] as const)("keeps a joined Server hidden when it checks in again because it %s", async (_case, machinePhase, failure) => {
  await foundOrganization();
  const { token } = await autoToken(5);
  const host = newHost();
  expect(await send(token, host, readyAuto)).toEqual(arrivalWait);
  const attempt = (await autoAttempts())[0]?.id ?? "";
  await endAuto(attempt, "joined");
  await harness.pool.query(`insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, machine_name)
    values ($1, repeat('a', 32), repeat('c', 64), '{}', 'web-old')`, [adaOrg]);
  await harness.pool.query("update server_enrollment set ended_at = now() - interval '1 hour'");
  const joined = { attemptId: attempt, stage: "joined" };
  const named = { ...readyAuto, attempt: failure === undefined ? joined : { ...joined, failure } };
  const again = machinePhase === undefined ? named : { ...named, machinePhase };

  expect(await send(token, host, again)).toEqual(wait);
  expect(await run(listWaitingServers(ada, { organizationSlug: "ada" }))).toEqual([]);
  expect(await autoAttempts()).toHaveLength(1);
});

it.each(["revoked", "expired", "swept"] as const)("refuses a second arrival after an attempt an older app inserted, once the host's row is %s", async (end) => {
  await foundOrganization();
  const { token, id } = await autoToken(5);
  const other = await run(createFleetToken(ada, { organizationSlug: "ada", name: "other", expiresIn: "30d", autoEnroll: null }));
  const host = newHost();
  expect(await send(token, host, { ...readyAuto, report: { blockers: [{ code: "disk_too_small", message: "Disk too small" }] } })).toEqual(wait);
  await harness.pool.query(
    `insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state, ended_at)
     values ($1, $2, $3, 'joiner', '{}', gen_random_uuid(), $4, now() - interval '8 days')`,
    [adaOrg, (await one()).id, id, end === "swept" ? "joined" : "failed"],
  );
  if (end === "swept") expect(await run(closeStaleServerEnrollments())).toEqual({ failed: 0, removed: 1 });
  else {
    expect(await send(other.token, host, readyAuto)).toEqual(wait);
    if (end === "revoked") await run(revokeFleetToken(ada, { organizationSlug: "ada", id: other.id }));
    else {
      await harness.pool.query("update fleet_token set expires_at = now() - interval '1 second' where id = $1", [other.id]);
      expect(await send(other.token, host, readyAuto)).toMatchObject({ kind: "stop", reason: "token_expired" });
    }
  }
  expect(await rows()).toHaveLength(0);
  expect(await send(token, host, readyAuto)).toEqual(wait);
  expect(await autoAttempts()).toHaveLength(0);
});

it("backfills the attempt markers the trigger writes, under each attempt's own token, from the enrollments already there", async () => {
  await foundOrganization();
  const { token, id: tokenId } = await autoToken(5);
  const retried = newHost();
  expect(await send(token, retried, readyAuto)).toEqual(arrivalWait);
  expect(await send(token, newHost(), readyAuto)).toEqual(arrivalWait);
  await endAuto((await autoAttempts())[0]?.id ?? "", "failed");
  const [row] = (await harness.pool.query<{ id: string }>("select id from waiting_server where checkin_key = $1", [retried.key])).rows;
  const input = { ids: [row?.id ?? ""], names: { kind: "hostnames" }, storage: { kind: "none" }, runs: { services: true, ingress: true, builds: true } } as const;
  await run(startServerEnrollment(ada, { organizationSlug: "ada", ...input }));
  const other = await run(createFleetToken(ada, { organizationSlug: "ada", name: "other", expiresIn: "30d", autoEnroll: null }));
  const moved = newHost();
  expect(await send(token, moved, readyAuto)).toEqual(arrivalWait);
  await endAuto((await autoAttempts())[3]?.id ?? "", "failed");
  expect(await send(other.token, moved, readyAuto)).toEqual(wait);
  expect(await autoAttempts()).toHaveLength(4);
  const markers = async () => (await harness.pool.query("select * from fleet_token_attempt order by checkin_key")).rows;
  const written = await markers();
  expect(written).toHaveLength(3);
  expect(written.every((marker) => marker.fleet_token_id === tokenId)).toBe(true);
  const migration = await readFile(new URL("../../../../drizzle/20261020084557_fleet_token_attempt/migration.sql", import.meta.url), "utf8");
  const backfill = migration.split("--> statement-breakpoint").find((statement) => statement.includes('INSERT INTO "fleet_token_attempt"')) ?? "";
  await harness.pool.query("delete from fleet_token_attempt");
  await harness.pool.query(backfill);
  expect(await markers()).toEqual(written);
});

it("keeps joined usage after the seven-day sweep and refuses another arrival", async () => {
  await foundOrganization();
  const { token } = await autoToken(1);
  const first = newHost();
  expect(await send(token, first, readyAuto)).toEqual(arrivalWait);
  const attempt = (await autoAttempts())[0];
  await endAuto(attempt?.id ?? "", "joined");
  expect(await run(endServerEnrollment(attempt?.id ?? "", `auto-${attempt?.id}`, { state: "joined", machineId: Schema.decodeUnknownSync(rustMachineIdSchema)("a".repeat(32)) }))).toBe(false);
  await harness.pool.query("update server_enrollment set ended_at = now() - interval '8 days'");
  expect(await run(closeStaleServerEnrollments())).toEqual({ failed: 0, removed: 1 });
  expect(await autoAttempts()).toHaveLength(0);
  expect(await rows()).toHaveLength(0);
  expect(await send(token, newHost(), readyAuto)).toEqual(wait);
  expect(await autoAttempts()).toHaveLength(0);
  expect((await run(listFleetTokens(ada, { organizationSlug: "ada" })))[0]).toMatchObject({ autoEnrollMax: 1, joined: 1, waiting: 1 });
});

it("keeps an open arrival running after revoke but refuses any new arrivals", async () => {
  await foundOrganization();
  const { token, id } = await autoToken();
  const first = newHost();
  expect(await send(token, first, readyAuto)).toEqual(arrivalWait);
  expect(await run(revokeFleetToken(ada, { organizationSlug: "ada", id }))).toEqual({ waitingRemoved: 0 });
  expect(await send(token, first, readyAuto)).toMatchObject({ kind: "enroll", role: "joiner" });
  expect(await send(token, newHost(), readyAuto)).toMatchObject({ kind: "stop", reason: "token_revoked" });
  expect(await autoAttempts()).toHaveLength(1);
});

// Holds a ready arrival host's row from another session, so a test can queue transactions behind it in a known order.
const holdArrivalHost = async (token: string) => {
  const host = newHost();
  await send(token, host, { ...readyAuto, prep: { docker: "installing", zfs: "ready" } });
  await harness.pool.query(`update waiting_server set report = jsonb_set(report, '{prep,docker}', '"ready"')`);
  const holder = await harness.pool.connect();
  await holder.query("begin");
  await holder.query("select id from waiting_server for update");
  const release = async () => {
    await holder.query("commit");
    holder.release();
  };
  return { host, release };
};
const lockWaiters = async (count: number) => {
  await expect.poll(async () => (await harness.pool.query<{ waiting: number }>(
    "select count(*)::int as waiting from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'",
  )).rows[0]?.waiting, { timeout: 5_000, interval: 10 }).toBeGreaterThanOrEqual(count);
};
const arrive = (id: string, host: Host) => run(startArrivalEnrollment({ id, organizationId: adaOrg, autoEnrollMax: 10 }, host.key));

it("lets a check-in overlap its host's arrival without deadlock", async () => {
  await foundOrganization();
  const { token, id } = await autoToken(10);
  const { host, release } = await holdArrivalHost(token);
  const arrival = arrive(id, host);
  await lockWaiters(1);
  const checkin = send(token, host, readyAuto);
  await lockWaiters(2);
  await release();
  expect(await arrival).toBe(true);
  expect(await checkin).toMatchObject({ kind: "enroll", role: "joiner" });
  expect(await autoAttempts()).toHaveLength(1);
});

it("starts one arrival when two overlap for the same host, and the other just waits", async () => {
  await foundOrganization();
  const { token, id } = await autoToken(10);
  const { host, release } = await holdArrivalHost(token);
  const first = arrive(id, host);
  await lockWaiters(1);
  const second = arrive(id, host);
  await lockWaiters(2);
  await release();
  expect([...(await Promise.all([first, second]))].sort()).toEqual([false, true]);
  expect(await autoAttempts()).toHaveLength(1);
});

it("answers an admitted check-in with a plain wait when its arrival fails", async () => {
  await foundOrganization();
  const { token } = await autoToken(10);
  await harness.pool.query(`create function refuse_enrollment() returns trigger language plpgsql as $$ begin raise exception 'refused'; end $$;
    create trigger refuse_enrollment before insert on server_enrollment execute function refuse_enrollment();`);
  try {
    expect(await send(token, newHost(), readyAuto)).toEqual(wait);
    expect(await rows()).toHaveLength(1);
  } finally {
    await harness.pool.query("drop trigger refuse_enrollment on server_enrollment; drop function refuse_enrollment();");
  }
});
