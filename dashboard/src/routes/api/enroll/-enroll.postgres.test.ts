import { it } from "@effect/vitest";
import { Effect, Schema } from "effect";
import { expect } from "vitest";
import type { JsonValue } from "#/db/schema";
import { rustMachineIdSchema } from "#/lib/schema";
import { asTestDouble } from "#/lib/test-double";
import { PostHog, type PostHogService } from "#/modules/analytics/posthog.server";
import { member } from "#/modules/identity/tables";
import { PloyzLive } from "#/modules/runtime/ployz.server";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { machineEnrollmentToken } from "#/modules/machines/tables";
import {
  handleMachineEnrollmentCallback,
  handleMachineEnrollmentJoin,
  handleMachineSetupReport,
} from "#/routes/api/enroll/-handlers";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const organizationId = "00000000-0000-4000-8000-000000000501";
const token = "pmet_secret";
const machineId = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const identity = {
  protocolVersion: 1 as const,
  machineId: "c".repeat(32),
  initialPolicy: { labels: {}, accepts_builds: true, accepts_services: true, accepts_ingress: true },
  name: "node-1",
  publicKey: "XQhwYRG/2fpuX4+RlNuIsE5SfhGdsGpMVVvwu1y2Ak0=",
  advertisedEndpoints: ["10.0.0.1:51820"],
  publicIp: "203.0.113.10",
  requestedStorage: "zfs" as const,
};
const headers = { "content-type": "application/json", authorization: `Bearer ${token}` };

const post = (path: string, body: JsonValue, init: Record<string, string> = headers) =>
  new Request(`https://cloud.example/api/enroll${path}`, { method: "POST", headers: init, body: JSON.stringify(body) });

const inCloud = <A, E, R>(body: Effect.Effect<A, E, R>) => Effect.gen(function* () {
  const cloud = yield* storeTestCloud();
  yield* seedStoreOrganization(organizationId).pipe(Effect.provide(cloud));
  return yield* body.pipe(
    Effect.provide(cloud),
    Effect.provide(PloyzLive),
    Effect.provideService(PostHog, asTestDouble<PostHogService>()({ capture: () => Effect.void })),
  );
});

const claimant = Schema.decodeUnknownSync(rustMachineIdSchema)(identity.machineId);

const issueToken = Effect.fn(function* (claimed?: { readonly joined: boolean }) {
  const { drizzle } = yield* Database;
  const [owner] = yield* drizzle.select({ userId: member.userId }).from(member).limit(1);
  yield* drizzle.insert(machineEnrollmentToken).values({
    organizationId,
    createdByUserId: owner?.userId ?? "",
    tokenHash: hashEnrollmentToken(token),
    expiresAt: new Date(Date.now() + 86_400_000),
    claimedMachineId: claimed ? claimant : null,
    claimedPublicKey: claimed ? identity.publicKey : null,
    claimedAt: claimed ? new Date() : null,
    joinedMachineId: claimed?.joined ? claimant : null,
  });
});

const bodyOf = (response: Response) => Effect.promise(() => response.text());

it.live("answers 401 to a join without a bearer token, whatever else it carries", () =>
  inCloud(Effect.gen(function* () {
    const json = { "content-type": "application/json" };
    const requests = [
      new Request("https://cloud.example/api/enroll", { method: "POST", headers: json, body: JSON.stringify(identity) }),
      new Request(`https://cloud.example/api/enroll/${token}`, { method: "POST", headers: json, body: JSON.stringify(identity) }),
      post("", identity, { ...json, authorization: token }),
      post("", identity, { ...json, authorization: "Bearer " }),
      post("", identity, { ...json, authorization: `Basic ${token}` }),
    ];
    for (const request of requests) {
      const response = yield* handleMachineEnrollmentJoin(request);
      expect(response.status, request.url).toBe(401);
      expect(yield* bodyOf(response)).not.toContain(token);
    }
  })));

it.live("answers callback and report without the bearer header with the plain unauthenticated error", () =>
  inCloud(Effect.gen(function* () {
    const unauthenticated = (path: string) => post(`/${token}${path}`, {}, { "content-type": "application/json" });
    for (const response of [
      yield* handleMachineEnrollmentCallback(unauthenticated("/callback")),
      yield* handleMachineSetupReport(unauthenticated("/report")),
    ]) {
      expect(response.status).toBe(401);
      expect(yield* bodyOf(response)).toBe(JSON.stringify({ error: "Authentication is required." }));
      expect(response.headers.get("Ployz-Error-Code")).toBeNull();
    }
  })));

it.live("answers 426 to a join that doesn't speak protocol version 1", () =>
  inCloud(Effect.gen(function* () {
    for (const body of [{ name: identity.name, publicKey: identity.publicKey }, { ...identity, protocolVersion: 2 }]) {
      const response = yield* handleMachineEnrollmentJoin(post("", body));
      expect(response.status).toBe(426);
      expect(yield* bodyOf(response)).toBe(JSON.stringify({
        error: "Unsupported enrollment protocol version; Cloud accepts version 1.",
      }));
      expect(response.headers.get("Ployz-Error-Code")).toBeNull();
    }
  })));

it.live("answers 422 to an identity that is incomplete, unselectable or has a malformed key", () =>
  inCloud(Effect.gen(function* () {
    const complete = { labels: { pool: "build" }, accepts_builds: true, accepts_services: false, accepts_ingress: false };
    const invalid: JsonValue[] = [
      { protocolVersion: 1, name: identity.name, publicKey: identity.publicKey, advertisedEndpoints: identity.advertisedEndpoints },
      { ...identity, initialPolicy: { labels: complete.labels, accepts_services: false, accepts_ingress: false } },
      { ...identity, initialPolicy: { ...complete, labels: { "rack/zone": "west" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: "" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: " west" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: "west\n" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: "🦀" } } },
      { ...identity, publicKey: "pk-founder" },
    ];
    for (const body of invalid) {
      const response = yield* handleMachineEnrollmentJoin(post("", body));
      expect(response.status, JSON.stringify(body)).toBe(422);
    }
  })));

it.live("answers an unknown token with a generic 401 that never names it", () =>
  inCloud(Effect.gen(function* () {
    const joined = yield* handleMachineEnrollmentJoin(post("", identity));
    expect(joined.status).toBe(401);
    expect(yield* bodyOf(joined)).not.toContain(token);
  })));

it.live("hands the first Machine its pairing and ignores identity fields from newer CLIs", () =>
  inCloud(Effect.gen(function* () {
    yield* issueToken();
    const response = yield* handleMachineEnrollmentJoin(post("", { ...identity, futureFact: true }));
    const text = yield* bodyOf(response);
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(JSON.parse(text)).toMatchObject({ kind: "initialize", resumed: false });
    expect(text).not.toContain(token);
  })));

it.live("answers a token claimed by another Machine as an unknown one", () =>
  inCloud(Effect.gen(function* () {
    yield* issueToken({ joined: false });
    const response = yield* handleMachineEnrollmentJoin(post("", {
      ...identity,
      machineId: "d".repeat(32),
      publicKey: "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=",
    }));
    expect(response.status).toBe(401);
    expect(response.headers.get("Ployz-Error-Code")).toBeNull();
    expect(yield* bodyOf(response)).not.toContain(token);
  })));

it.live("answers 422 to a callback that is prefixed, carries extras or lacks its fields", () =>
  inCloud(Effect.gen(function* () {
    const bodies: JsonValue[] = [
      { machineId: `org_${machineId}`, pairingCredential: "ppair_secret" },
      { machineId, pairingCredential: "ppair_secret", extra: true },
      {},
      { stage: "publish", machineId, pairingCredential: "ppair_secret", capability: "" },
      { stage: "publish", machineId, pairingCredential: "ppair_secret", capability: "x".repeat(94) },
      { machineId, pairingCredential: "ppair_secret", sealing: { kind: "held", publicKey: "short" } },
      { machineId, pairingCredential: "ppair_secret", sealing: { kind: "lost" } },
    ];
    for (const body of bodies) {
      const response = yield* handleMachineEnrollmentCallback(post("/callback", body));
      expect(response.status, JSON.stringify(body)).toBe(422);
      expect(yield* bodyOf(response)).not.toContain("ppair_secret");
    }
  })));

it.live("answers a callback for an unknown token with a generic failure that never names it", () =>
  inCloud(Effect.gen(function* () {
    const response = yield* handleMachineEnrollmentCallback(post("/callback", { machineId, pairingCredential: "ppair_secret" }));
    expect(response.status).toBeGreaterThanOrEqual(400);
    const text = yield* bodyOf(response);
    expect(text).not.toContain(token);
    expect(text).not.toContain("ppair_secret");
  })));

const succeeded = {
  outcome: "succeeded",
  profile: { arch: "x86_64" },
  steps: [{ name: "install", seconds: 3.5 }],
  totalSeconds: 3.5,
};

it.live("records a setup report from the Machine that joined and answers no content", () =>
  inCloud(Effect.gen(function* () {
    yield* issueToken({ joined: true });
    const response = yield* handleMachineSetupReport(post("/report", succeeded));
    expect(response.status).toBe(204);
    expect(response.headers.get("cache-control")).toBe("no-store");
  })));

it.live("answers a report that doesn't fit its token with 404", () =>
  inCloud(Effect.gen(function* () {
    yield* issueToken();
    const response = yield* handleMachineSetupReport(post("/report", succeeded));
    expect(response.status).toBe(404);
    expect(yield* bodyOf(response)).not.toContain(token);
  })));

it.live("answers 422 to setup reports with unknown outcomes, steps or numbers", () =>
  inCloud(Effect.gen(function* () {
    yield* issueToken({ joined: true });
    const invalid: JsonValue[] = [
      {},
      { ...succeeded, outcome: "maybe" },
      { ...succeeded, steps: [{ name: "reboot", seconds: 1 }] },
      { ...succeeded, steps: [{ name: "install", seconds: -1 }] },
      { ...succeeded, totalSeconds: "10" },
      { ...succeeded, profile: { cpuCount: 1.5 } },
      { ...succeeded, outcome: "failed" },
      { ...succeeded, outcome: "failed", failedStep: "reboot", failedStepSeconds: 1, error: "x" },
    ];
    for (const body of invalid) {
      const response = yield* handleMachineSetupReport(post("/report", body));
      expect(response.status, JSON.stringify(body)).toBe(422);
    }
  })));
