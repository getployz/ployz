import { createHash, generateKeyPairSync, randomBytes, type KeyObject, sign } from "node:crypto";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { InngestClient } from "#/modules/inngest/client";
import type { CheckinRequest } from "#/modules/machines/waiting/checkin-wire";
import { checkinMessage } from "#/modules/machines/waiting/checkin.server";
import { createFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { handleCheckin, observedIp } from "#/routes/api/checkin/-handler";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const ada = { userId: "00000000-0000-4000-8000-000000000701" };

let harness: PostgresTestHarness;
let token: string;
const inngest = new Inngest({ id: "checkin-route-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

beforeEach(async () => {
  await harness.pool.query(`
    truncate table checkin_rate_limit, waiting_server, fleet_token, "user", organization cascade;
    insert into organization (id, name, slug) values ('00000000-0000-4000-8000-000000000711', 'Ada', 'ada');
    insert into "user" (id, email, name) values ('${ada.userId}', 'ada@example.test', 'Ada');
    insert into member (id, organization_id, user_id, role, created_at) values
      (gen_random_uuid(), '00000000-0000-4000-8000-000000000711', '${ada.userId}', 'owner', now());
  `);
  ({ token } = await harness.runEffect(
    createFleetToken(ada, { organizationSlug: "ada", name: "rack-1", expiresIn: "1d", autoEnroll: null }),
  ));
});

const post = (body: Buffer, headers: Record<string, string>) =>
  harness.runEffect(Effect.provideService(handleCheckin(
    new Request("http://cloud.test/api/checkin", { method: "POST", body: body.toString("latin1"), headers }),
    "203.0.113.7",
  ), InngestClient, inngest));

const signed = (bearer: string, body: Uint8Array, { publicKey, privateKey }: { publicKey: KeyObject; privateKey: KeyObject } = generateKeyPairSync("ed25519")) => {
  const key = Buffer.from(publicKey.export({ format: "jwk" }).x ?? "", "base64url").toString("hex");
  const json = Buffer.from(JSON.stringify({ ...JSON.parse(Buffer.from(body).toString()), checkinKey: key }));
  const timestamp = BigInt(Math.floor(Date.now() / 1000));
  return {
    json,
    headers: {
      authorization: `Bearer ${bearer}`,
      "ployz-timestamp": String(timestamp),
      "ployz-signature": `v1=${sign(null, checkinMessage(key, timestamp, json), privateKey).toString("base64url")}`,
    },
  };
};

const hostReport = {
  hostname: "web-1",
  os: { id: "debian", version: "12" },
  kernel: "6.1.0",
  arch: "aarch64",
  virt: "none",
  cpuCount: 2,
  memoryTotalBytes: 4_294_967_296,
  disks: [],
  docker: { kind: "absent" },
  cgroup: "v2",
  zfs: { kind: "ineligible", code: "no_disk", message: "No empty disk." },
  blockers: [],
  warnings: [],
};
const checkin = Buffer.from(JSON.stringify({ protocolVersion: 1, cliVersion: "0.3.0", report: hostReport }));

const count = async () => Number((await harness.pool.query("select count(*) from waiting_server")).rows[0]?.count);

it("answers a signed check-in with wait, uncached", async () => {
  const { json, headers } = signed(token, checkin);
  const response = await post(json, headers);
  expect(response.status).toBe(200);
  expect(response.headers.get("cache-control")).toBe("no-store");
  expect(await response.json()).toEqual({ kind: "wait", retryAfter: 10 });
  expect(await count()).toBe(1);
});

it("answers an unknown token with 401 and writes nothing", async () => {
  const { json, headers } = signed("pflt_unknown", checkin);
  const response = await post(json, headers);
  expect(response.status).toBe(401);
  expect(await count()).toBe(0);
});

it("answers a body its signature doesn't cover with 401, and a signed malformed one with 400", async () => {
  const { json, headers } = signed(token, checkin);
  expect((await post(json.subarray(0, -1), headers)).status).toBe(401);
  const malformed = signed(token, Buffer.from(JSON.stringify({ protocolVersion: 1 })));
  expect((await post(malformed.json, malformed.headers)).status).toBe(400);
  expect(await count()).toBe(0);
});

it("refuses a body over 64 KiB with 413 before reading it all", async () => {
  const big = Buffer.from(JSON.stringify({ ...JSON.parse(checkin.toString()), padding: "x".repeat(65 * 1024) }));
  const { json, headers } = signed(token, big);
  const response = await post(json, headers);
  expect(response.status).toBe(413);
  expect(await count()).toBe(0);
});

it("takes the observed address from the proxy header, else the socket peer", () => {
  const headers = new Headers({ "x-forwarded-for": "198.51.100.9, 198.51.100.4, 10.0.0.1", "x-real-ip": "198.51.100.5" });
  expect(observedIp(headers, undefined, "10.0.0.2")).toBe("198.51.100.4");
  expect(observedIp(headers, ["x-real-ip"], "10.0.0.2")).toBe("198.51.100.5");
  expect(observedIp(new Headers({ "x-forwarded-for": "not-an-ip" }), undefined, "10.0.0.2")).toBe("10.0.0.2");
  expect(observedIp(new Headers(), undefined, undefined)).toBe("unknown");
});

it("trusts only the last value of the client address header", () => {
  const spoofed = new Headers([["x-real-ip", "198.51.100.66"], ["x-real-ip", "203.0.113.7"]]);
  expect(observedIp(spoofed, ["x-real-ip"], "10.0.0.2")).toBe("203.0.113.7");
  expect(observedIp(new Headers({ "x-real-ip": "198.51.100.66, 203.0.113.7" }), ["x-real-ip"], "10.0.0.2")).toBe("203.0.113.7");
});

it("ignores a forwarded address that did not come through a proxy", () => {
  const headers = new Headers({ "x-forwarded-for": "198.51.100.4" });
  expect(observedIp(headers, undefined, "203.0.113.7")).toBe("203.0.113.7");
  expect(observedIp(headers, undefined, "::ffff:127.0.0.1")).toBe("198.51.100.4");
  expect(observedIp(headers, undefined, "::1")).toBe("198.51.100.4");
});

it("takes the client from the trusted end of an appended forwarded chain", () => {
  const appended = new Headers({ "x-forwarded-for": "198.51.100.99, 203.0.113.7" });
  expect(observedIp(appended, undefined, "127.0.0.1")).toBe("203.0.113.7");
  expect(observedIp(appended, undefined, "172.17.0.1")).toBe("203.0.113.7");
  expect(observedIp(new Headers({ "x-forwarded-for": "10.0.0.5, 10.0.0.1" }), undefined, "10.0.0.2")).toBe("10.0.0.5");
});

it("strips the port a proxy appends to a forwarded hop", () => {
  expect(observedIp(new Headers({ "x-forwarded-for": "198.51.100.99, 203.0.113.7:5678" }), undefined, "127.0.0.1")).toBe("203.0.113.7");
  expect(observedIp(new Headers({ "x-forwarded-for": "[2001:db8::7]:5678, 10.0.0.1:443" }), undefined, "::1")).toBe("2001:db8::7");
  expect(observedIp(new Headers({ "x-forwarded-for": "[2001:db8::7]" }), undefined, "::1")).toBe("2001:db8::7");
});

const adaOrg = "00000000-0000-4000-8000-000000000711";
const sealingKeys = async () => (await harness.pool.query("select cluster_key, public_key from cluster_sealing_key")).rows;

const pair = (secret: string) => harness.runEffect(Effect.gen(function* () {
  const encryption = yield* SecretEncryption;
  yield* Effect.promise(() => harness.pool.query(
    `insert into organization_pairing (organization_id, founder_machine_id, founder_claim_machine_id, founder_public_key, encrypted_pairing_secret)
     values ($1, repeat('a', 32), repeat('a', 32), repeat('b', 64), $2)
     on conflict (organization_id) do update set encrypted_pairing_secret = excluded.encrypted_pairing_secret`,
    [adaOrg, JSON.stringify(encryption.encrypt(secret))],
  ));
}));

const joinedHost = async () => {
  const host = generateKeyPairSync("ed25519");
  const first = signed(token, checkin, host);
  await post(first.json, first.headers);
  const machineId = randomBytes(16).toString("hex");
  const { rows: [enrolled] } = await harness.pool.query<{ id: string }>(
    `with server as (
       insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability)
       values ($1, $2, repeat('c', 64), '{}')
     )
     insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state, machine_id, ended_at)
     select organization_id, id, fleet_token_id, 'joiner', '{}', gen_random_uuid(), 'joined', $2, now() from waiting_server
     order by first_seen_at desc limit 1 returning id`,
    [adaOrg, machineId],
  );
  return async (sealing: CheckinRequest["sealing"], attemptId = enrolled?.id, stage = "publishing") => {
    const body = Buffer.from(JSON.stringify({ ...JSON.parse(checkin.toString()), attempt: { attemptId, stage }, sealing }));
    const { json, headers } = signed(token, body, host);
    expect((await post(json, headers)).status).toBe(200);
  };
};

const held = (byte: number) => ({ kind: "held", publicKey: Buffer.alloc(32, byte).toString("base64") }) as const;
const generation = (secret: string) => createHash("sha256").update(secret).digest("hex");

it("records the first Sealing Key a pairing generation's Servers report, and keeps it", async () => {
  await pair("pairing-1");
  const report = await joinedHost();
  await report(held(1));
  await report(held(1));
  expect(await sealingKeys()).toEqual([{ cluster_key: generation("pairing-1"), public_key: held(1).publicKey }]);

  await (await joinedHost())(held(2));
  expect(await sealingKeys()).toEqual([{ cluster_key: generation("pairing-1"), public_key: held(1).publicKey }]);
});

it("files each pairing generation's Sealing Key apart", async () => {
  await pair("pairing-1");
  await (await joinedHost())(held(1));
  await pair("pairing-2");
  await (await joinedHost())(held(2));
  expect(await sealingKeys()).toEqual(expect.arrayContaining([
    { cluster_key: generation("pairing-1"), public_key: held(1).publicKey },
    { cluster_key: generation("pairing-2"), public_key: held(2).publicKey },
  ]));
});

it("records nothing for a Server that needs access, has not committed its join, names another host's attempt, or has no pairing", async () => {
  await pair("pairing-1");
  const report = await joinedHost();
  await report({ kind: "needs_access" });
  await report(held(1), undefined, "storage");
  await report(held(1), "00000000-0000-4000-8000-000000000000");
  await report(held(1), "not-an-attempt");
  const [theirs] = (await harness.pool.query<{ id: string }>("select id from server_enrollment")).rows;
  await (await joinedHost())(held(1), theirs?.id);
  expect(await sealingKeys()).toEqual([]);

  await harness.pool.query("delete from organization_pairing");
  await report(held(1));
  expect(await sealingKeys()).toEqual([]);
});

it("answers the check-in even when its Sealing Key report cannot be recorded", async () => {
  await pair("pairing-1");
  await harness.pool.query("update organization_pairing set encrypted_pairing_secret = '{}'");
  await (await joinedHost())(held(1));
  expect(await sealingKeys()).toEqual([]);
});
