import { describe, expect, it } from "vitest";
import { Option, Schema } from "effect";
import {
  buildMachineJoinCommand,
  enrollmentExpiry,
  enrollmentIdentitySchema,
  type EnrollmentTokenRow,
  mintedEnrollment,
  redactIpAddresses,
  registerRequestFromEnrollmentIdentity,
  ResetPendingEnrollmentInput,
  rustMachineIdSchema,
  setupReportAccepted,
  setupReportProperties,
  setupReportSchema,
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

describe("setup report", () => {
  const now = new Date("2026-10-01T00:00:00.000Z");
  const pending: EnrollmentTokenRow = {
    userId: "user-1",
    organizationId: "org-1",
    joinedMachineId: null,
    expiresAt: new Date(now.getTime() + 60_000),
  };
  const joined = { ...pending, joinedMachineId: machineId };
  const expired = { ...pending, expiresAt: now };
  const decode = Schema.decodeUnknownSync(setupReportSchema);

  it("accepts success only for a joined token and failure only for a pending one", () => {
    const cases: [EnrollmentTokenRow | undefined, "succeeded" | "failed", boolean][] = [
      [joined, "succeeded", true],
      [pending, "succeeded", false],
      [undefined, "succeeded", false],
      [pending, "failed", true],
      [joined, "failed", false],
      [expired, "failed", false],
      [undefined, "failed", false],
    ];
    for (const [row, outcome, accepted] of cases) {
      expect(setupReportAccepted(row, outcome, now), `${outcome} ${JSON.stringify(row)}`).toBe(accepted);
    }
  });

  it("flattens a success into snake_case profile and step properties", () => {
    const report = decode({
      outcome: "succeeded",
      profile: {
        provider: "hetzner",
        instanceType: "CX22",
        osId: "debian",
        osVersion: "13",
        kernel: "6.12.0",
        arch: "x86_64",
        virtualization: "kvm",
        cpuCount: 2,
        memoryTotalBytes: 4_294_967_296,
        diskTotalBytes: 42_949_672_960,
        storage: "zfs",
        ployzVersion: "0.2.1",
        founder: true,
        futureFact: "dropped",
      },
      steps: [
        { name: "install", seconds: 3.5 },
        { name: "enroll", seconds: 1 },
        { name: "storage", seconds: 960 },
        { name: "join", seconds: 12 },
      ],
      totalSeconds: 976.5,
      failedStep: "storage",
    });

    expect(setupReportProperties(report)).toEqual({
      provider: "hetzner",
      instance_type: "CX22",
      os_id: "debian",
      os_version: "13",
      kernel: "6.12.0",
      arch: "x86_64",
      virtualization: "kvm",
      cpu_count: 2,
      memory_total_bytes: 4_294_967_296,
      disk_total_bytes: 42_949_672_960,
      storage: "zfs",
      ployz_version: "0.2.1",
      founder: true,
      step_install_seconds: 3.5,
      step_enroll_seconds: 1,
      step_storage_seconds: 960,
      step_join_seconds: 12,
      total_seconds: 976.5,
    });
  });

  it("adds the failure and truncates long strings rather than rejecting them", () => {
    const report = decode({
      outcome: "failed",
      profile: { osId: "x".repeat(5_000), storage: "btrfs" },
      steps: [{ name: "install", seconds: 3.5 }],
      totalSeconds: 30,
      failedStep: "enroll",
      failedStepSeconds: 25.5,
      error: "e".repeat(5_000),
    });

    expect(setupReportProperties(report)).toEqual({
      os_id: "x".repeat(256),
      storage: "btrfs",
      step_install_seconds: 3.5,
      total_seconds: 30,
      failed_step: "enroll",
      failed_step_seconds: 25.5,
      error: "e".repeat(1_000),
    });
  });
});

describe("setup report error", () => {
  it("redacts IP addresses and keeps hostnames and versions", () => {
    expect(redactIpAddresses("failed to connect to root@203.0.113.10:22")).toBe("failed to connect to root@<ip>:22");
    expect(redactIpAddresses("no route to 2001:db8::1")).toBe("no route to <ip>");
    expect(redactIpAddresses("dial [fe80::1%eth0]:22 refused")).toBe("dial [<ip>]:22 refused");
    expect(redactIpAddresses("addr:2001:db8::1 refused")).toBe("addr:<ip> refused");
    expect(redactIpAddresses("tcp6:fe80::1")).toBe("tcp6:<ip>");
    expect(redactIpAddresses("x_10.0.0.1 down")).toBe("x_<ip> down");
    expect(redactIpAddresses("could not reach 10.0.0.1.")).toBe("could not reach <ip>.");
    expect(redactIpAddresses("2001:0db8:0000:0000:0000:ff00:0042:8329 down")).toBe("<ip> down");
    const kept = "ployz 0.2.1 on kernel 6.12.43 at db.example.com, 12:34:56, std::io::Error, aa:bb:cc:dd:ee:ff, 1.2.3.4.5";
    expect(redactIpAddresses(kept)).toBe(kept);
  });

  it("redacts before truncating", () => {
    const report = Schema.decodeUnknownSync(setupReportSchema)({
      outcome: "failed",
      profile: {},
      steps: [],
      totalSeconds: 1,
      failedStep: "install",
      failedStepSeconds: 1,
      error: `${"x".repeat(990)} 203.0.113.10`,
    });

    expect(report.outcome === "failed" && report.error).toBe(`${"x".repeat(990)} <ip>`);
  });
});
