import { generateKeyPairSync, type KeyObject, sign } from "node:crypto";
import { Cause, Effect, Exit } from "effect";
import { afterAll, beforeAll, beforeEach, expect, it } from "vitest";
import type { CheckinRequest, HostReport, Prep } from "#/modules/machines/waiting/checkin-wire";
import { checkIn, checkinMessage, WAITING_PER_TOKEN } from "#/modules/machines/waiting/checkin.server";
import { createFleetToken, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import {
  listWaitingServers,
  showWaitingServer,
  waitingServerStatus,
} from "#/modules/machines/waiting/waiting-servers.server";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ada = { userId: "00000000-0000-4000-8000-000000000601" };
const bob = { userId: "00000000-0000-4000-8000-000000000602" };
const adaOrg = "00000000-0000-4000-8000-000000000611";

let harness: PostgresTestHarness;

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
  harness.runEffect(createFleetToken(ada, { organizationSlug: "ada", name, expiresIn: "1d", autoEnroll: null }));

type Host = { readonly key: string; readonly privateKey: KeyObject };

const newHost = (): Host => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const x = publicKey.export({ format: "jwk" }).x ?? "";
  return { key: Buffer.from(x, "base64url").toString("hex"), privateKey };
};

type Send = {
  readonly hostname?: string;
  readonly prep?: Prep;
  readonly skew?: number;
  readonly badSignature?: boolean;
  readonly ip?: string;
};

const request = (token: string, host: Host, options: Send = {}) => {
  const base = {
    protocolVersion: 1,
    checkinKey: host.key,
    cliVersion: "0.3.0",
    report: { ...report, hostname: options.hostname ?? report.hostname },
  } as const;
  const checkin: CheckinRequest = options.prep === undefined ? base : { ...base, prep: options.prep };
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

const send = (token: string, host: Host, options?: Send) => harness.runEffect(checkIn(request(token, host, options)));

const failure = async <A, E>(operation: Effect.Effect<A, E, Database>) => {
  const exit = await harness.runEffect(Effect.exit(operation));
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

it("refuses a body that isn't a check-in with 400", async () => {
  const { token } = await fleetToken("rack-1");
  const signed = request(token, newHost());
  const body = Buffer.from(JSON.stringify({ protocolVersion: 2 }));
  expect(await failure(checkIn({ ...signed, body }))).toMatchObject({ _tag: "CheckinRefused", status: 400 });
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
  expect(await send(revoking.token, second)).toEqual({ kind: "stop", reason: "token_revoked", message: "The Fleet Token was revoked." });
  expect(await rows()).toEqual([]);
});

it("keeps a rejected host stopped under its token, and lets a new token admit it again", async () => {
  const first = await fleetToken("rack-1");
  const host = newHost();
  await send(first.token, host);
  const row = await one();
  expect(await harness.runEffect(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [row.id] }))).toEqual({ rejected: 1 });
  expect(await harness.runEffect(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [row.id] }))).toEqual({ rejected: 0 });
  expect(await harness.runEffect(listWaitingServers(ada, { organizationSlug: "ada" }))).toEqual([]);

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
      harness.runEffect(Effect.exit(checkIn(request(token, newHost(), { ip: `198.51.100.${n + 1}` })))),
    ),
  );
  expect(outcomes.filter((exit) => Exit.isSuccess(exit))).toHaveLength(2);
  expect(await rows()).toHaveLength(WAITING_PER_TOKEN);
});

it("rate limits one source address and keeps what it already wrote", async () => {
  const { token } = await fleetToken("rack-1");
  const hosts = Array.from({ length: 120 }, newHost);
  const outcomes = [];
  for (const host of hosts) outcomes.push(await harness.runEffect(Effect.exit(checkIn(request(token, host, { ip: "198.51.100.1" })))));
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
  expect(await harness.runEffect(revokeFleetToken(ada, { organizationSlug: "ada", id }))).toEqual({ waitingRemoved: 3 });
  expect(await rows()).toHaveLength(1);
});

it("lists and shows waiting hosts only to their own Organization", async () => {
  const { token } = await fleetToken("rack-1");
  await send(token, newHost(), { hostname: "<script>alert(1)</script>" });
  const [listed] = await harness.runEffect(listWaitingServers(ada, { organizationSlug: "ada" }));
  const id = listed?.id ?? "";
  expect(listed).toMatchObject({ fleetToken: "rack-1", status: "waiting", report: { hostname: "<script>alert(1)</script>" } });
  expect(await harness.runEffect(showWaitingServer(ada, { organizationSlug: "ada", id }))).toEqual(listed);
  expect(await failure(showWaitingServer(bob, { organizationSlug: "bob", id })))
    .toMatchObject({ _tag: "NotFound", message: "No such waiting server." });
  expect(await harness.runEffect(rejectWaitingServers(bob, { organizationSlug: "bob", ids: [id] }))).toEqual({ rejected: 0 });
});

it("reads status from the last check-in and the report", () => {
  const now = new Date("2026-10-16T00:01:00Z");
  const recent = new Date("2026-10-16T00:00:50Z");
  expect(waitingServerStatus(report, new Date("2026-10-16T00:00:29Z"), now)).toBe("offline");
  expect(waitingServerStatus({ ...report, blockers: [{ code: "cgroup_v1", message: "cgroup v1." }] }, recent, now)).toBe("blocked");
  expect(waitingServerStatus({ ...report, prep: { docker: "failed", zfs: "off" } }, recent, now)).toBe("preparing");
  expect(waitingServerStatus({ ...report, prep: { docker: "ready", zfs: "installing" } }, recent, now)).toBe("waiting");
});
