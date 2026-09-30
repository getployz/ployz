import { describe, expect, it } from "vitest";
import { Option, Schema } from "effect";
import {
  buildMachineJoinCommand,
  enrollmentExpiry,
  enrollmentIdentitySchema,
  mintedEnrollment,
  registerRequestFromEnrollmentIdentity,
  ResetPendingEnrollmentInput,
  rustMachineIdSchema,
  waitForFounder,
} from "#/modules/machines/enrollment";

const machineId = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const display = "XQhwYRG/2fpuX4+RlNuIsE5SfhGdsGpMVVvwu1y2Ak0=";

describe("machine enrollment command", () => {
  it("installs the stable release on hosted Cloud", () => {
    const minted = mintedEnrollment({
      origin: "https://ployz.dev",
      token: "pmet_secret",
      version: "1.2.3-beta.4",
      expiresAt: new Date("2026-08-19T00:00:00.000Z"),
    });

    expect(minted.command).toBe(
      "curl -fsSL https://ployz.sh/ | sh && sudo ployz server add --token 'pmet_secret'",
    );
    expect(minted.expiresAt).toBe("2026-08-19T00:00:00.000Z");
  });

  it("pins its own version and adds --cloud-url for self-hosted Cloud", () => {
    expect(
      buildMachineJoinCommand({
        token: "pmet_secret",
        origin: "https://cloud.example",
        version: "1.2.3",
      }),
    ).toBe(
      "curl -fsSL https://ployz.sh/ | sh -s -- 1.2.3 && sudo ployz server add --token 'pmet_secret' --cloud-url 'https://cloud.example'",
    );
  });

  it("keeps token expiry stable", () => {
    expect(enrollmentExpiry(new Date("2026-08-18T00:00:00.000Z"))).toEqual(
      new Date("2026-08-19T00:00:00.000Z"),
    );
  });
});

describe("organization enrollment server-function inputs", () => {
  it("requires exact reset confirmation", () => {
    expect(
      Option.isSome(Schema.decodeUnknownOption(ResetPendingEnrollmentInput)({
        organizationSlug: "acme",
        confirmedFounderStoppedOrErased: true,
      })),
    ).toBe(true);
    expect(
      Option.isSome(Schema.decodeUnknownOption(ResetPendingEnrollmentInput)({
        organizationSlug: "acme",
        confirmedFounderStoppedOrErased: false,
      })),
    ).toBe(false);
  });
});

describe("versioned enrollment identity", () => {
  it.each(["West", "EU west:/alpha.*()?+[]\\^$|_-"])("carries selectable initial policy %s into registration", (value) => {
    const initialPolicy = {
      labels: { "rack.zone": value },
      accepts_builds: true,
      accepts_services: false,
      accepts_ingress: false,
    };
    const identity = Schema.decodeUnknownSync(enrollmentIdentitySchema)({
      protocolVersion: 1,
      machineId: Schema.decodeUnknownSync(rustMachineIdSchema)("c".repeat(32)),
      name: "builder",
      publicKey: display,
      advertisedEndpoints: ["203.0.113.10:51820"],
      requestedStorage: "none",
      initialPolicy,
    });
    expect(registerRequestFromEnrollmentIdentity(identity).initial_policy).toEqual(
      initialPolicy,
    );
  });

  it("requires protocol version 1 and a rust Machine identity", () => {
    const valid = {
      protocolVersion: 1,
      machineId: Schema.decodeUnknownSync(rustMachineIdSchema)("c".repeat(32)),
      initialPolicy: {
        labels: {},
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
      },
      name: "node-1",
      publicKey: display,
      advertisedEndpoints: ["203.0.113.10:51820"],
      requestedStorage: "none",
    };
    expect(
      Option.isSome(Schema.decodeUnknownOption(enrollmentIdentitySchema)(valid)),
    ).toBe(true);
    expect(
      Option.isNone(
        Schema.decodeUnknownOption(enrollmentIdentitySchema)({
          ...valid,
          protocolVersion: 2,
        }),
      ),
    ).toBe(true);
    expect(
      Option.isSome(Schema.decodeUnknownOption(rustMachineIdSchema)(machineId)),
    ).toBe(true);
    expect(
      Option.isNone(
        Schema.decodeUnknownOption(rustMachineIdSchema)(`org_${machineId}`),
      ),
    ).toBe(true);
  });

  it.each([null, "203.0.113.10"])("maps enrollment public IP %s without Cloud allocation", (publicIp) => {
    const request = registerRequestFromEnrollmentIdentity({
      protocolVersion: 1,
      machineId: Schema.decodeUnknownSync(rustMachineIdSchema)("c".repeat(32)),
      initialPolicy: {
        labels: {},
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
      },
      name: "node-1",
      publicKey: display,
      advertisedEndpoints: ["203.0.113.10:51820"],
      publicIp,
      requestedStorage: "zfs",
    });
    expect(request.public_ip).toBe(publicIp ?? null);
    expect(request.storage).toBe("zfs");
    expect(request.public_key).toHaveLength(32);
    expect(JSON.stringify(request)).not.toMatch(/\/24/u);
  });

  it("waits for two seconds without advertising a claim expiry", () => {
    expect(waitForFounder()).toEqual({ kind: "not_yet", retryAfter: 2 });
  });
});
