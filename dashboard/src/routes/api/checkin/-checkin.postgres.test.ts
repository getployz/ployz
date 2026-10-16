import { generateKeyPairSync, sign } from "node:crypto";
import { afterAll, beforeAll, beforeEach, expect, it } from "vitest";
import { checkinMessage } from "#/modules/machines/waiting/checkin.server";
import { createFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { handleCheckin, observedIp } from "#/routes/api/checkin/-handler";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ada = { userId: "00000000-0000-4000-8000-000000000701" };

let harness: PostgresTestHarness;
let token: string;

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
  harness.runEffect(handleCheckin(
    new Request("http://cloud.test/api/checkin", { method: "POST", body: body.toString("latin1"), headers }),
    "203.0.113.7",
  ));

const signed = (bearer: string, body: Uint8Array) => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
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
