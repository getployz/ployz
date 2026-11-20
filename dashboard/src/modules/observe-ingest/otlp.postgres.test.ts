import { createPrivateKey, sign } from "node:crypto";
import { gzipSync } from "node:zlib";
import { it } from "@effect/vitest";
import { asc, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterEach, expect, vi } from "vitest";
import { organizationMachine } from "#/modules/machines/tables";
import { crashEvent } from "#/modules/observe-ingest/tables";
import { operationOutbox } from "#/modules/operations/tables";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { handleMachineOtlp } from "#/routes/api/machines/-handlers";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f301";
const OTHER_ORGANIZATION = "00000000-0000-4000-8000-00000000f302";
const MACHINE = machineId("0");
const SEED = "07".repeat(32);
const PUBLIC_KEY = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const OTHER_SEED = "08".repeat(32);
const COLLECTOR = "http://observe-collector:4318";

const capability = (publicKey: string) => `ployz1:${Buffer.concat([Buffer.from(publicKey, "hex"), Buffer.alloc(32, 9)]).toString("base64url")}`;
const privateKey = (seed: string) =>
  createPrivateKey({ key: Buffer.from(`302e020100300506032b657004220420${seed}`, "hex"), format: "der", type: "pkcs8" });

type Minting = { readonly machine?: string; readonly seed?: string; readonly iat?: number; readonly exp?: number };

const mint = ({ machine = MACHINE, seed = SEED, iat = Math.floor(Date.now() / 1000), exp = iat + 3600 }: Minting = {}) => {
  const payload = Buffer.from(JSON.stringify({ machine, iat, exp })).toString("base64url");
  const signature = sign(null, Buffer.concat([Buffer.from("ployz-otlp-v1\0"), Buffer.from(payload)]), privateKey(seed));
  return `Bearer ployz-otlp-v1.${payload}.${signature.toString("base64url")}`;
};

it("the known-answer token is what M1 mints: these exact bytes verify under the vector's key", () => {
  const authorization = "Bearer ployz-otlp-v1.eyJtYWNoaW5lIjoiYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWEiLCJpYXQiOjE3NjAwMDAwMDAsImV4cCI6MTc2MDAwMzYwMH0"
    + ".tvurJ9YIYWfDYv2nWDQXUC3ReUY_Gudf5AP6xl-I5Ss4LSxig5XCSahPtYYTdMrLhU0TxS301gunh2hRzbFqAQ";
  expect(mint({ machine: "a".repeat(32), iat: 1_760_000_000, exp: 1_760_003_600 })).toBe(authorization);
});

const body = JSON.stringify({ resourceLogs: [] });

const setup = Effect.fn(function* (env: Readonly<Record<string, string>> = { OBSERVE_COLLECTOR_URL: COLLECTOR }) {
  const inngest = new Inngest({ id: "observe-otlp-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const services = yield* storeTestCloud({ inngest, env });
  yield* seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }).pipe(Effect.provide(services));
  yield* enrollStoreServer(ORGANIZATION).pipe(Effect.provide(services));
  // The fixture's capability is not a real one; give the Machine the vector's key.
  yield* Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    yield* drizzle.update(organizationMachine).set({ encryptedCapability: encryption.encrypt(capability(PUBLIC_KEY)), managementIdentity: PUBLIC_KEY });
  }).pipe(Effect.provide(services), Effect.orDie);

  const collector = vi.spyOn(globalThis, "fetch").mockImplementation(async () => new Response("{}", { status: 200, headers: { "content-type": "application/json" } }));
  const post = (
    authorization: string | undefined,
    headers: Record<string, string> = {},
    signal: "logs" | "metrics" | "traces" = "logs",
    payload: string | Uint8Array<ArrayBuffer> = body,
  ) => {
    const sent = new Headers({ "content-type": "application/json", ...headers });
    if (authorization !== undefined) sent.set("authorization", authorization);
    const request = new Request(`https://cloud.example.com/api/machines/otlp/v1/${signal}`, { method: "POST", body: payload, headers: sent });
    return handleMachineOtlp(request, signal).pipe(Effect.provide(services));
  };
  const enrollElsewhere = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    yield* drizzle.insert(organization).values({ id: OTHER_ORGANIZATION, name: "Other", slug: "other" });
    yield* drizzle.insert(organizationPairing).values({
      organizationId: OTHER_ORGANIZATION, encryptedPairingSecret: encryption.encrypt("ppair_other"), founderPublicKey: "founder-key", founderClaimMachineId: MACHINE,
    });
    yield* drizzle.insert(organizationMachine).values({
      organizationId: OTHER_ORGANIZATION, machineId: MACHINE, clusterKey: "e".repeat(64), encryptedCapability: encryption.encrypt(capability(PUBLIC_KEY)),
      managementIdentity: PUBLIC_KEY, isDialEntry: true,
    });
  }).pipe(Effect.provide(services), Effect.orDie);
  const removeMachine = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.delete(organizationMachine).where(eq(organizationMachine.machineId, MACHINE));
  }).pipe(Effect.provide(services), Effect.orDie);
  const ledger = () => Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return {
      crashes: (yield* drizzle.select().from(crashEvent).orderBy(asc(crashEvent.eventId)))
        .map(({ eventId, organizationId, machineId, serviceId, serviceName, exitCode, oom }) => ({ eventId, organizationId, machineId, serviceId, serviceName, exitCode, oom })),
      mails: (yield* drizzle.select().from(operationOutbox)).filter(({ name }) => name === "observe/crash.received").map(({ data }) => data),
    };
  }).pipe(Effect.provide(services), Effect.orDie);
  const breakLedger = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`alter table crash_event rename to crash_event_gone`);
  }).pipe(Effect.provide(services), Effect.orDie);
  return { post, collector, enrollElsewhere, removeMachine, ledger, breakLedger };
});

afterEach(() => vi.restoreAllMocks());

it.live("a verified Machine's batch reaches the Collector stamped with its Organization, whatever the sender claims", () => Effect.gen(function* () {
  const { post, collector } = yield* setup();
  const answer = yield* post(mint(), { "x-ployz-org": "someone-else", "x-ployz-retention-days": "9999", "x-ployz-machine": "spoof" }, "traces");
  expect(answer.status).toBe(200);
  expect(collector).toHaveBeenCalledOnce();
  const [url, init] = collector.mock.calls[0] ?? [];
  expect(url).toBe(`${COLLECTOR}/v1/traces`);
  expect(Object.fromEntries(new Headers(init?.headers))).toEqual({
    "content-type": "application/json",
    "x-ployz-org": ORGANIZATION,
    "x-ployz-retention-days": "30",
  });
}), 60_000);

it.live("a token another Machine's key signed, an expired one, one from the future, and a malformed one get 401 and reach no Collector", () => Effect.gen(function* () {
  const { post, collector } = yield* setup();
  const now = Math.floor(Date.now() / 1000);
  const refused = [
    mint({ seed: OTHER_SEED }),
    mint({ iat: now - 7200, exp: now - 3600 }),
    mint({ iat: now + 3600, exp: now + 7200 }),
    mint({ iat: now, exp: now + 90_000 }),
    mint().slice(0, -2),
    "Bearer nonsense",
    undefined,
  ];
  for (const authorization of refused) expect((yield* post(authorization)).status).toBe(401);
  expect(collector).not.toHaveBeenCalled();
}), 60_000);

it.live("a Machine the Organization removed, and one held by two Organizations, get 401", () => Effect.gen(function* () {
  const { post, collector, enrollElsewhere, removeMachine } = yield* setup();
  yield* enrollElsewhere;
  expect((yield* post(mint())).status).toBe(401);
  yield* removeMachine;
  expect((yield* post(mint())).status).toBe(401);
  expect(collector).not.toHaveBeenCalled();
}), 60_000);

it.live("a Cloud with no Collector, one that is down, and one that refuses the body answer 503, 503 and 400", () => Effect.gen(function* () {
  const none = yield* setup({});
  expect((yield* none.post(mint())).status).toBe(503);
  const { post, collector } = yield* setup();
  collector.mockRejectedValueOnce(new TypeError("fetch failed"));
  expect((yield* post(mint())).status).toBe(503);
  collector.mockResolvedValueOnce(new Response("bad", { status: 400 }));
  expect((yield* post(mint())).status).toBe(400);
}), 60_000);

const WEB = "c".repeat(32);
const API = "d".repeat(32);
const EXIT = "ployz.container.exit";

type Value = { readonly stringValue: string } | { readonly boolValue: boolean } | { readonly intValue: string };
const text = (stringValue: string): Value => ({ stringValue });
const flag = (boolValue: boolean): Value => ({ boolValue });
const int = (value: number): Value => ({ intValue: String(value) });

const exit = (containerId: string, at: bigint, extra: Readonly<Record<string, Value | undefined>> = {}) => ({
  timeUnixNano: String(at),
  observedTimeUnixNano: String(at),
  severityNumber: 17,
  severityText: "ERROR",
  body: { stringValue: "web-1 exited with code 1" },
  eventName: EXIT,
  attributes: Object.entries({
    "event.name": text(EXIT),
    "ployz.event.id": text(`${containerId}:${at}`),
    "ployz.machine.id": text(MACHINE),
    "ployz.namespace": text("prod"),
    "ployz.service.name": text("web"),
    "ployz.service.id": text(WEB),
    "container.id": text(containerId),
    "ployz.replica": text("1"),
    "ployz.exit.code": int(1),
    "ployz.exit.oom": flag(false),
    ...extra,
  }).flatMap(([key, value]) => (value === undefined ? [] : [{ key, value }])),
});

const logs = (...logRecords: ReadonlyArray<unknown>) => JSON.stringify({ resourceLogs: [{ resource: { attributes: [] }, scopeLogs: [{ logRecords }] }] });
const AT = 1_760_000_000_000_000_000n;
const CONTAINER = "b".repeat(64);

it.live("the same crash event posted twice is forwarded twice and queues one mail", () => Effect.gen(function* () {
  const { post, collector, ledger } = yield* setup();
  const crash = logs(exit(CONTAINER, AT));
  expect((yield* post(mint(), {}, "logs", crash)).status).toBe(200);
  expect((yield* post(mint(), {}, "logs", crash)).status).toBe(200);
  expect(collector).toHaveBeenCalledTimes(2);
  expect(yield* ledger()).toEqual({
    crashes: [{ eventId: `${CONTAINER}:${AT}`, organizationId: ORGANIZATION, machineId: MACHINE, serviceId: WEB, serviceName: "prod/web", exitCode: 1, oom: false }],
    mails: [{ organizationId: ORGANIZATION, serviceId: WEB }],
  });
}), 60_000);

it.live("a batch of crashes queues one mail per Service, and a gzip body counts the same", () => Effect.gen(function* () {
  const { post, ledger } = yield* setup();
  const batch = logs(
    exit(CONTAINER, AT),
    exit(CONTAINER, AT + 1n, { "ployz.exit.code": int(137), "ployz.exit.oom": flag(true) }),
    exit("e".repeat(64), AT, { "ployz.service.id": text(API), "ployz.service.name": text("api"), "ployz.exit.code": undefined }),
  );
  const answer = yield* post(mint(), { "content-encoding": "gzip" }, "logs", new Uint8Array(gzipSync(batch)));
  expect(answer.status).toBe(200);
  const { crashes, mails } = yield* ledger();
  expect(crashes.map(({ serviceName, exitCode, oom }) => ({ serviceName, exitCode, oom }))).toEqual([
    { serviceName: "prod/web", exitCode: 1, oom: false },
    { serviceName: "prod/web", exitCode: 137, oom: true },
    { serviceName: "prod/api", exitCode: null, oom: false },
  ]);
  expect(mails).toHaveLength(2);
}), 60_000);

it.live("a body that names another Organization and Machine mails only the token's Organization", () => Effect.gen(function* () {
  const { post, ledger } = yield* setup();
  const spoof = exit(CONTAINER, AT, {
    "ployz.machine.id": text(machineId("1")),
    "ployz.organization.id": text(OTHER_ORGANIZATION),
    organization_id: text(OTHER_ORGANIZATION),
  });
  expect((yield* post(mint(), { "x-ployz-org": OTHER_ORGANIZATION }, "logs", logs(spoof))).status).toBe(200);
  expect(yield* ledger()).toEqual({
    crashes: [expect.objectContaining({ organizationId: ORGANIZATION, machineId: MACHINE })],
    mails: [{ organizationId: ORGANIZATION, serviceId: WEB }],
  });
}), 60_000);

it.live("records that are not crashes, or lack what a crash needs, are forwarded and ignored beside one that is", () => Effect.gen(function* () {
  const { post, collector, ledger } = yield* setup();
  const batch = logs(
    { timeUnixNano: String(AT), body: { stringValue: "a line from a log file" }, attributes: [{ key: "ployz.service.name", value: { stringValue: "web" } }] },
    exit(CONTAINER, AT + 1n, { "event.name": text("something.else") }),
    exit(CONTAINER, AT + 2n, { "ployz.service.id": undefined }),
    exit(CONTAINER, AT + 3n, { "ployz.exit.oom": undefined }),
    exit(CONTAINER, AT + 4n, { "ployz.event.id": text("") }),
    exit(CONTAINER, AT + 5n, { "event.name": undefined }),
    { ...exit(CONTAINER, AT + 6n), unknownField: { nested: true } },
  );
  expect((yield* post(mint(), {}, "logs", batch)).status).toBe(200);
  expect(collector).toHaveBeenCalledOnce();
  expect((yield* ledger()).crashes.map(({ eventId }) => eventId)).toEqual([`${CONTAINER}:${AT + 6n}`]);
}), 60_000);

it.live("bodies that cannot hold a crash, and logs that are not JSON, are forwarded with no mail", () => Effect.gen(function* () {
  const { post, collector, ledger } = yield* setup();
  const crash = logs(exit(CONTAINER, AT));
  const bomb = new Uint8Array(gzipSync(`{"resourceLogs":[],"pad":"${" ".repeat(9 * 1024 * 1024)}"}`));
  expect((yield* post(mint(), { "content-type": "application/x-protobuf" }, "logs", crash)).status).toBe(200);
  expect((yield* post(mint(), { "content-encoding": "zstd" }, "logs", crash)).status).toBe(200);
  expect((yield* post(mint(), {}, "logs", "{not json")).status).toBe(200);
  expect((yield* post(mint(), { "content-encoding": "gzip" }, "logs", bomb)).status).toBe(200);
  expect((yield* post(mint(), { "content-encoding": "gzip" }, "logs", "plainly not gzip")).status).toBe(200);
  expect((yield* post(mint(), {}, "metrics", crash)).status).toBe(200);
  expect(collector).toHaveBeenCalledTimes(6);
  expect(yield* ledger()).toEqual({ crashes: [], mails: [] });
}), 60_000);

it.live("a crash the Collector did not take is not kept, and one it took but the ledger could not keep answers 503", () => Effect.gen(function* () {
  const { post, collector, ledger, breakLedger } = yield* setup();
  const crash = logs(exit(CONTAINER, AT));
  collector.mockResolvedValueOnce(new Response("bad", { status: 400 }));
  expect((yield* post(mint(), {}, "logs", crash)).status).toBe(400);
  collector.mockRejectedValueOnce(new TypeError("fetch failed"));
  expect((yield* post(mint(), {}, "logs", crash)).status).toBe(503);
  expect(yield* ledger()).toEqual({ crashes: [], mails: [] });

  yield* breakLedger;
  expect((yield* post(mint(), {}, "logs", crash)).status).toBe(503);
  expect(collector).toHaveBeenCalledTimes(3);
}), 60_000);

it.live("a crash batch with a bad token reaches no Collector and keeps nothing", () => Effect.gen(function* () {
  const { post, collector, ledger } = yield* setup();
  expect((yield* post(mint({ seed: OTHER_SEED }), {}, "logs", logs(exit(CONTAINER, AT)))).status).toBe(401);
  expect(collector).not.toHaveBeenCalled();
  expect(yield* ledger()).toEqual({ crashes: [], mails: [] });
}), 60_000);
