import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import {
  confirmedVolumeRemove,
  unionDataLossLists,
} from "#/modules/runtime/data-loss-confirm";
import {
  cloudEnvironmentName,
  confirmedVolumesForTeardown,
  environmentCloudRows,
  identitiesForEnvironment,
  identitiesForMachine,
  leftoverVolumeMessage,
  machineCloudRow,
  organizationCloudRow,
  projectCloudRow,
  environmentTargetsWithIdentities,
  planTeardownRuntime,
  parseTeardownTargets,
  retryPlanForAttempt,
  teardownCompletedDescription,
  teardownOutcome,
  type TeardownTargets,
} from "./teardown";

const machineA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;
const machineB = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" as MachineId;

function rustVolume(machine_id: MachineId, name: string) {
  return { kind: "docker_volume" as const, id: { machine_id, name } };
}

describe("teardown Data Loss", () => {
  it("unions five environments into one list for a Cloud project delete", () => {
    const names = [
      "production",
      "staging",
      "preview",
      "dev",
      "qa",
    ] as const;
    const lists = names.map((environmentName, index) => {
      const cloudName = cloudEnvironmentName({
        organizationSlug: "acme",
        projectSlug: "web",
        environmentName,
      });
      return unionDataLossLists([
        {
          rust: [
            rustVolume(
              index === 1 ? machineB : machineA,
              `vol-env-${environmentName}`,
            ),
          ],
          cloud: [],
        },
        environmentCloudRows({
          cloudName,
          services: [{ name: "api" }],
          volumes: [{ name: "pg-data" }],
        }),
      ]);
    });

    const united = unionDataLossLists([
      ...lists,
      projectCloudRow({ organizationSlug: "acme", projectSlug: "web" }),
    ]);

    expect(united.rust).toHaveLength(5);
    expect(united.cloud.filter((row) => row.kind === "environment")).toEqual([
      { kind: "environment", name: "acme/web/production" },
      { kind: "environment", name: "acme/web/staging" },
      { kind: "environment", name: "acme/web/preview" },
      { kind: "environment", name: "acme/web/dev" },
      { kind: "environment", name: "acme/web/qa" },
    ]);
    expect(united.cloud.some((row) => row.kind === "project")).toBe(true);
  });

  it("gives each environment only its confirmed volume identities", () => {
    const environments = [
      { id: "env-1", projectId: "project-1", name: "production", namespace: "ns-prod" },
      { id: "env-2", projectId: "project-1", name: "staging", namespace: "ns-staging" },
    ];
    const identities = [
      rustVolume(machineA, "vol-production"),
      rustVolume(machineB, "vol-staging"),
    ];
    const ownership = [
      { namespace: "ns-prod", machine: machineA, name: "vol-production" },
      { namespace: "ns-staging", machine: machineB, name: "vol-staging" },
    ];

    expect(
      identitiesForEnvironment(identities, "ns-staging", ownership),
    ).toEqual([rustVolume(machineB, "vol-staging")]);

    const targets = environmentTargetsWithIdentities({
      environments: environments.map((environment) => ({
        environmentId: environment.id,
        projectId: environment.projectId,
        namespace: environment.namespace,
        cloudName: `acme/web/${environment.name}`,
      })),
      identities,
      ownership,
    });
    expect(targets[0]?.identities).toEqual([
      rustVolume(machineA, "vol-production"),
    ]);
    expect(targets[1]?.identities).toEqual([
      rustVolume(machineB, "vol-staging"),
    ]);
  });

  it("filters confirmed identities down to one machine for removeMachine", () => {
    const identities = [
      rustVolume(machineA, "pg-data"),
      rustVolume(machineB, "pg-data"),
    ];

    expect(identitiesForMachine(identities, machineA)).toEqual([
      rustVolume(machineA, "pg-data"),
    ]);
  });

  it("turns confirmed rust identities into SDK volume ids", () => {
    expect(
      confirmedVolumesForTeardown([
        rustVolume(machineA, "vol-1"),
        rustVolume(machineB, "vol-2"),
      ]),
    ).toEqual(confirmedVolumeRemove({
      rust: [
        rustVolume(machineA, "vol-1"),
        rustVolume(machineB, "vol-2"),
      ],
      cloud: [],
    }));
  });

  it("treats leftover volume identities as retryable, not complete", () => {
    const requested = [
      { machine_id: machineA, name: "vol-1" },
      { machine_id: machineB, name: "vol-1" },
    ];
    const [destroyed, omitted] = requested;
    if (destroyed === undefined || omitted === undefined) {
      throw new Error("fixture is missing volume identities");
    }
    expect(
      leftoverVolumeMessage(requested, {
        destroyed: [destroyed],
        failed: [],
        omitted: [omitted],
      }),
    ).toMatch(/retry/);
    expect(
      leftoverVolumeMessage(requested, {
        destroyed: requested,
        failed: [],
        omitted: [],
      }),
    ).toBeNull();
  });

  it("resends pending teardown and retries terminal leftovers", () => {
    expect(retryPlanForAttempt("pending")).toEqual({ kind: "resend" });
    expect(retryPlanForAttempt("partial")).toEqual({ kind: "retry" });
    expect(retryPlanForAttempt("failed")).toEqual({ kind: "retry" });
    expect(retryPlanForAttempt("cancelled")).toEqual({ kind: "retry" });
    expect(retryPlanForAttempt("running")).toEqual({ kind: "conflict" });
    expect(retryPlanForAttempt("completed")).toEqual({ kind: "conflict" });
  });

  it("snapshots org pairing revoke on targets so retry works after Cloud rows drop", () => {
    const targets: TeardownTargets = {
      environments: [
        {
          environmentId: "env-1",
          projectId: "project-1",
          namespace: "ns-1",
          cloudName: "acme/web/production",
          identities: [],
        },
      ],
      machines: [machineA],
      revokePairing: true,
      runtimeMembership: "verified",
    };
    expect(targets.revokePairing).toBe(true);
    expect(organizationCloudRow("acme").cloud).toEqual([
      { kind: "organization", name: "acme" },
    ]);
    expect(machineCloudRow(machineA).cloud).toEqual([
      { kind: "machine", name: machineA },
    ]);
  });

  it("pins live Cluster machines for reachable org teardown and refuses to call that zero", () => {
    expect(
      planTeardownRuntime({
        scope: "organization",
        abandon: false,
        cluster: { kind: "live", machines: [machineA, machineB] },
      }),
    ).toEqual({
      kind: "ok",
      machines: [machineA, machineB],
      revokePairing: true,
      runtimeMembership: "verified",
    });
    expect(
      planTeardownRuntime({
        scope: "organization",
        abandon: false,
        cluster: { kind: "unreachable" },
      }),
    ).toEqual({ kind: "refuse", reason: "use_abandon" });
    expect(teardownOutcome("verified", false)).toEqual({
      rustMustRevokePairing: false,
      runtimeMembership: "verified_zero",
    });
  });

  it("abandons only an unreachable Cluster and records unknown membership", () => {
    expect(
      planTeardownRuntime({
        scope: "organization",
        abandon: true,
        cluster: { kind: "unreachable" },
      }),
    ).toEqual({
      kind: "ok",
      machines: [],
      revokePairing: true,
      runtimeMembership: "unknown",
    });
    expect(
      planTeardownRuntime({
        scope: "organization",
        abandon: true,
        cluster: { kind: "live", machines: [machineA] },
      }),
    ).toEqual({ kind: "refuse", reason: "use_verified" });
    expect(
      planTeardownRuntime({
        scope: "organization",
        abandon: true,
        cluster: { kind: "no_cluster" },
      }),
    ).toEqual({ kind: "refuse", reason: "nothing_to_abandon" });
    expect(teardownOutcome("unknown", false)).toEqual({
      rustMustRevokePairing: false,
      runtimeMembership: "unknown",
    });
  });

  it("leaves env and project teardown off Cluster membership", () => {
    expect(
      planTeardownRuntime({
        scope: "project",
        abandon: false,
        cluster: { kind: "live", machines: [machineA] },
      }),
    ).toEqual({
      kind: "ok",
      machines: [],
      revokePairing: false,
      runtimeMembership: "untouched",
    });
  });

  it("describes verified zero separately from unknown runtime membership", () => {
    expect(
      teardownCompletedDescription({
        rustMustRevokePairing: false,
        runtimeMembership: "verified_zero",
      }),
    ).toBe("The cluster was removed. Cloud recorded verified zero.");
    expect(
      teardownCompletedDescription({
        rustMustRevokePairing: true,
        runtimeMembership: "unknown",
      }),
    ).toBe(
      "Cloud management was dropped. Runtime membership remains unknown, and pairing must still be revoked in Rust.",
    );
    expect(teardownOutcome("verified", true)).toEqual({
      rustMustRevokePairing: true,
      runtimeMembership: "unknown",
    });
    expect(teardownCompletedDescription(null)).toBe(
      "Confirmed rust work ran, then Cloud rows were dropped.",
    );
  });

  it("treats attempt JSON missing runtimeMembership as untouched", () => {
    expect(
      parseTeardownTargets({
        environments: [],
        machines: [],
        revokePairing: false,
      }).runtimeMembership,
    ).toBe("untouched");
  });
});
