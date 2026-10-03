import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import type { JsonValue } from "#/db/schema";
import { NotFound, Validation, Unauthorized } from "#/server/public-error";
import { Effect } from "effect";
import {
  handleMachineEnrollmentCallback,
  handleMachineEnrollmentJoin,
  handleMachineSetupReport,
} from "#/routes/api/enroll/-handlers";
import type { SetupReport } from "#/modules/machines/enrollment";

const mocks = {
  enroll: vi.fn(),
  completeFounding: vi.fn(),
};

const token = "pmet_secret";
const machineId = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const identity = {
  protocolVersion: 1 as const,
  machineId: "c".repeat(32),
  initialPolicy: {
    labels: {},
    accepts_builds: true,
    accepts_services: true,
    accepts_ingress: true,
  },
  name: "node-1",
  publicKey: "XQhwYRG/2fpuX4+RlNuIsE5SfhGdsGpMVVvwu1y2Ak0=",
  advertisedEndpoints: ["10.0.0.1:51820"],
  publicIp: "203.0.113.10",
  requestedStorage: "zfs" as const,
  memoryTotalBytes: 8_589_934_592,
  diskTotalBytes: 107_374_182_400,
  diskAvailableBytes: 85_899_345_920,
};
const waiterIdentity = {
  ...identity,
  publicKey: "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=",
};

function join(body: JsonValue, extra?: RequestInit) {
  return Effect.runPromise(handleMachineEnrollmentJoin(
    new Request("https://cloud.example/api/enroll/pmet_secret", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      ...extra,
    }),
    token,
    mocks.enroll,
  ));
}

function callback(body: JsonValue, extra?: RequestInit) {
  return Effect.runPromise(handleMachineEnrollmentCallback(
    new Request("https://cloud.example/api/enroll/pmet_secret/callback", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      ...extra,
    }),
    token,
    mocks.completeFounding,
  ));
}

describe("machine enrollment routes", () => {
  beforeEach(() => vi.clearAllMocks());

  it("returns initialize pairing for the first machine without leaking the token", async () => {
    mocks.enroll.mockReturnValue(
      Effect.succeed({
        kind: "initialize",
        resumed: false,
        pairing: {
          secret: "ppair_secret",
        },
        storage: "zfs",
      }),
    );

    const response = await join(identity);
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(mocks.enroll).toHaveBeenCalledWith({
      token,
      identity,
    });
    expect(body).toEqual({
      kind: "initialize",
      resumed: false,
      pairing: {
        secret: "ppair_secret",
      },
      storage: "zfs",
    });
    expect(JSON.stringify(body)).not.toContain(token);
  });

  it("returns not_yet for a concurrent public key without leaking the token", async () => {
    mocks.enroll.mockReturnValue(
      Effect.succeed({
        kind: "not_yet",
        retryAfter: 2,
      }),
    );

    const response = await join(waiterIdentity);
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(body).toEqual({
      kind: "not_yet",
      retryAfter: 2,
    });
    expect(JSON.stringify(body)).not.toContain(token);
  });

  it("returns join pairing and Register payload when a connection candidate is available without leaking the token", async () => {
    const registration = {
      assigned_machine: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      visible_peers: ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
      target_versions: {},
    };
    mocks.enroll.mockReturnValue(
      Effect.succeed({
        kind: "join",
        pairing: {
          secret: "ppair_secret",
        },
        storage: "zfs",
        registration,
      }),
    );

    const response = await join(waiterIdentity);
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(mocks.enroll).toHaveBeenCalledWith({
      token,
      identity: waiterIdentity,
    });
    expect(body).toEqual({
      kind: "join",
      pairing: {
        secret: "ppair_secret",
      },
      storage: "zfs",
      registration,
    });
    expect(JSON.stringify(body)).not.toContain(token);
    expect(JSON.stringify(body)).not.toMatch(/10\.\d+\.\d+\.\d+\/24/u);
  });

  it("rejects a missing enrollment token without reading identity", async () => {
    const response = await Effect.runPromise(
      handleMachineEnrollmentJoin(
        new Request("https://cloud.example/api/enroll/", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(identity),
        }),
        "",
        mocks.enroll,
      ),
    );
    const body = await response.text();

    expect(response.status).toBe(401);
    expect(mocks.enroll).not.toHaveBeenCalled();
    expect(body).not.toContain(token);
  });

  it("ignores unknown identity fields from newer CLIs", async () => {
    mocks.enroll.mockReturnValue(
      Effect.succeed({ kind: "not_yet", retryAfter: 2 }),
    );

    const response = await join({ ...identity, futureFact: true });

    expect(response.status).toBe(200);
    expect(mocks.enroll).toHaveBeenCalledWith({ token, identity });
  });

  it("rejects missing or unselectable initial policy before enrollment side effects", async () => {
    mocks.enroll.mockReturnValue(Effect.succeed({ kind: "not_yet", retryAfter: 2 }));
    const complete = {
      labels: { pool: "build" },
      accepts_builds: true,
      accepts_services: false,
      accepts_ingress: false,
    };
    const invalidBodies: JsonValue[] = [
      {
        protocolVersion: identity.protocolVersion,
        name: identity.name,
        publicKey: identity.publicKey,
        advertisedEndpoints: identity.advertisedEndpoints,
      },
      { ...identity, initialPolicy: { labels: complete.labels, accepts_services: false, accepts_ingress: false } },
      { ...identity, initialPolicy: { ...complete, labels: { "rack/zone": "west" } } },
      { ...identity, initialPolicy: { ...complete, labels: { "région": "west" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: "" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: " west" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: "west " } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: "é" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: "🦀" } } },
      { ...identity, initialPolicy: { ...complete, labels: { pool: "west\n" } } },
    ];
    for (const body of invalidBodies) {
      const response = await join(body);
      expect(response.status, JSON.stringify(body)).toBe(422);
    }
    expect(mocks.enroll).not.toHaveBeenCalled();
  });

  it("rejects incomplete or non-Display publicKey identity bodies", async () => {
    for (const response of [
      await join({
        protocolVersion: 1,
      machineId: "c".repeat(32),
        initialPolicy: {
          labels: {},
          accepts_builds: true,
          accepts_services: true,
          accepts_ingress: true,
        },
        name: identity.name,
        publicKey: identity.publicKey,
      }),
      await join({ ...identity, publicKey: "pk-founder" }),
    ]) {
      expect(response.status).toBe(422);
    }
    expect(mocks.enroll).not.toHaveBeenCalled();
  });

  it("requires protocol version 1 before granting a founding directive", async () => {
    for (const body of [
      {
        name: identity.name,
        publicKey: identity.publicKey,
        advertisedEndpoints: identity.advertisedEndpoints,
      },
      { ...identity, protocolVersion: 2 },
    ]) {
      const response = await join(body);
      expect(response.status).toBe(426);
      await expect(response.json()).resolves.toEqual({
        error: "Unsupported enrollment protocol version; Cloud accepts version 1.",
      });
    }
    expect(mocks.enroll).not.toHaveBeenCalled();
  });

  it("accepts enroll identity without publicIp", async () => {
    mocks.enroll.mockReturnValue(
      Effect.succeed({
        kind: "initialize",
        resumed: false,
        pairing: {
          secret: "ppair_secret",
        },
        storage: "none",
      }),
    );

    const response = await join({
      protocolVersion: 1,
      machineId: "c".repeat(32),
      initialPolicy: {
        labels: {},
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
      },
      name: identity.name,
      publicKey: identity.publicKey,
      advertisedEndpoints: identity.advertisedEndpoints,
    });

    expect(response.status).toBe(200);
    expect(mocks.enroll).toHaveBeenCalledWith({
      token,
      identity: {
        protocolVersion: 1,
      machineId: "c".repeat(32),
        initialPolicy: {
          labels: {},
          accepts_builds: true,
          accepts_services: true,
          accepts_ingress: true,
        },
        name: identity.name,
        publicKey: identity.publicKey,
        advertisedEndpoints: identity.advertisedEndpoints,
        requestedStorage: "none",
      },
    });
  });

  it("completes founding with the Pairing Credential and Machine id", async () => {
    mocks.completeFounding.mockReturnValue(
      Effect.succeed({ machineId }),
    );

    const response = await callback({
      machineId,
      pairingCredential: "ppair_secret",
    });
    const body = await response.json();

    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(mocks.completeFounding).toHaveBeenCalledWith({
      token,
      machineId,
      pairingCredential: "ppair_secret",
    });
    expect(body).toEqual({ machineId });
    expect(JSON.stringify(body)).not.toContain(token);
  });

  it("accepts a bounded protected candidate without reflecting capabilities", async () => {
    mocks.completeFounding.mockReturnValue(Effect.succeed({ machineId }));
    const published = { stage: "publish", machineId, pairingCredential: "ppair_secret", capability: "private-capability" };
    const accepted = await callback(published);
    expect(accepted.status).toBe(200);
    expect(mocks.completeFounding).toHaveBeenCalledWith({ token, ...published });
    expect(await accepted.json()).toEqual({ machineId });
    mocks.completeFounding.mockClear();
    for (const capability of ["", "x".repeat(94)]) {
      const rejected = await callback({ ...published, capability });
      expect(rejected.status).toBe(422);
      expect(await rejected.text()).not.toContain("private-capability");
    }
    expect(mocks.completeFounding).not.toHaveBeenCalled();
  });

  it("rejects prefixed or extra callback bodies", async () => {
    for (const response of [
      await callback({
        machineId: `org_${machineId}`,
        pairingCredential: "ppair_secret",
      }),
      await callback({
        machineId,
        pairingCredential: "ppair_secret",
        extra: true,
      }),
      await callback({}),
    ]) {
      expect(response.status).toBe(422);
    }
    expect(mocks.completeFounding).not.toHaveBeenCalled();
  });

  it("maps invalid tokens to generic unauthorized failures", async () => {
    mocks.enroll.mockReturnValue(
      Effect.fail(new Unauthorized()),
    );
    mocks.completeFounding.mockReturnValue(
      Effect.fail(new Validation({ message: "MachineId must be a 32-hex UUID." })),
    );

    const joinResponse = await join(identity);
    const callbackResponse = await callback({
      machineId: "nope",
      pairingCredential: "ppair_secret",
    });
    const joinBody = await joinResponse.text();
    const callbackBody = await callbackResponse.text();

    expect(joinResponse.status).toBe(401);
    expect(callbackResponse.status).toBe(422);
    expect(joinBody).not.toContain(token);
    expect(callbackBody).not.toContain(token);
  });
});

describe("setup report route", () => {
  const succeeded = {
    outcome: "succeeded",
    profile: { arch: "x86_64" },
    steps: [{ name: "install", seconds: 3.5 }],
    totalSeconds: 3.5,
  };

  type RecordReport = (input: { readonly token: string; readonly report: SetupReport }) => Effect.Effect<undefined, NotFound>;

  function report(body: JsonValue, record: Mock<RecordReport> = vi.fn<RecordReport>(() => Effect.succeed(undefined))) {
    const handled = handleMachineSetupReport(
      new Request("https://cloud.example/api/enroll/pmet_secret/report", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      }),
      token,
      record,
    );
    return Effect.runPromise(handled).then((response) => ({ response, record }));
  }

  it("records a valid report and answers no content", async () => {
    const { response, record } = await report(succeeded);

    expect(response.status).toBe(204);
    expect(record).toHaveBeenCalledWith({ token, report: succeeded });
  });

  it("answers not found when the token doesn't fit the outcome", async () => {
    const { response } = await report(
      succeeded,
      vi.fn<RecordReport>(() => Effect.fail(new NotFound({ message: "Enrollment not found." }))),
    );

    expect(response.status).toBe(404);
    expect(await response.text()).not.toContain(token);
  });

  it("rejects invalid bodies without recording them", async () => {
    const invalidBodies: JsonValue[] = [
      {},
      { ...succeeded, outcome: "maybe" },
      { ...succeeded, steps: [{ name: "reboot", seconds: 1 }] },
      { ...succeeded, steps: [{ name: "install", seconds: -1 }] },
      { ...succeeded, totalSeconds: "10" },
      { ...succeeded, profile: { cpuCount: 1.5 } },
      { ...succeeded, outcome: "failed" },
      { ...succeeded, outcome: "failed", failedStep: "reboot", failedStepSeconds: 1, error: "x" },
    ];
    for (const body of invalidBodies) {
      const { response, record } = await report(body);
      expect(response.status, JSON.stringify(body)).toBe(422);
      expect(record).not.toHaveBeenCalled();
    }
  });
});
