import type { ClusterTeardown, MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { unionDataLossLists } from "#/modules/runtime/data-loss-confirm";
import {
  cloudEnvironmentName,
  environmentCloudRows,
  incompleteTeardownOutcome,
  organizationCloudRow,
  parseTeardownTargets,
  planTeardownRuntime,
  projectCloudRow,
  retryPlanForAttempt,
  teardownCompletedDescription,
  teardownIsRetryable,
  teardownOutcome,
  type TeardownTargets,
} from "./teardown";

const machineA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;
const machineB = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" as MachineId;
const clusterTeardown = {
  destroyed_projects: ["app-production"],
  machines: { successes: [], failures: [], omissions: [] },
  pairing_revoked: false,
} satisfies ClusterTeardown;

describe("teardown Data Loss", () => {
  it("unions Rust project evidence with Cloud rows for a Cloud project delete", () => {
    const names = ["production", "staging", "preview", "dev", "qa"] as const;
    const lists = names.map((environmentName, index) => {
      const cloudName = cloudEnvironmentName({
        organizationSlug: "acme",
        projectSlug: "web",
        environmentName,
      });
      return unionDataLossLists([
        {
          rust: [
            {
              kind: "docker_volume" as const,
              id: {
                machine_id: index === 1 ? machineB : machineA,
                name: `vol-env-${environmentName}`,
              },
            },
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

  it("keeps Cloud runtime project targets without reconstructing volume ownership", () => {
    const targets: TeardownTargets = {
      environments: [
        {
          environmentId: "env-1",
          projectId: "project-1",
          projectName: "app-production",
          cloudName: "acme/app/production",
        },
      ],
      destroyRuntimeProjects: true,
      revokePairing: false,
      runtimeMembership: "untouched",
    };

    expect(targets.environments).toEqual([
      {
        environmentId: "env-1",
        projectId: "project-1",
        projectName: "app-production",
        cloudName: "acme/app/production",
      },
    ]);
    expect(targets.destroyRuntimeProjects).toBe(true);
  });

  it("only resends a pending dispatch; terminal work needs a fresh confirmation", () => {
    expect(retryPlanForAttempt("pending")).toEqual({ kind: "resend" });
    expect(teardownIsRetryable("pending")).toBe(true);
    for (const status of ["partial", "failed", "cancelled", "running", "completed"] as const) {
      expect(retryPlanForAttempt(status)).toEqual({ kind: "conflict" });
      expect(teardownIsRetryable(status)).toBe(false);
    }
  });

  it("keeps organization Cloud row loss without inventing a machine list", () => {
    expect(organizationCloudRow("acme").cloud).toEqual([
      { kind: "organization", name: "acme" },
    ]);
  });

  it("uses the reachable Rust cluster target without pinning Cloud ownership", () => {
    expect(
      planTeardownRuntime({
        scope: "organization",
        abandon: false,
        cluster: { kind: "live" },
      }),
    ).toEqual({
      kind: "ok",
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
      revokePairing: true,
      runtimeMembership: "unknown",
    });
    expect(
      planTeardownRuntime({
        scope: "organization",
        abandon: true,
        cluster: { kind: "live" },
      }),
    ).toEqual({ kind: "refuse", reason: "use_verified" });
    expect(
      planTeardownRuntime({
        scope: "organization",
        abandon: true,
        cluster: { kind: "no_cluster" },
      }),
    ).toEqual({ kind: "refuse", reason: "nothing_to_abandon" });
  });

  it("leaves environment and project teardown off Cluster membership", () => {
    expect(
      planTeardownRuntime({
        scope: "project",
        abandon: false,
        cluster: { kind: "live" },
      }),
    ).toEqual({
      kind: "ok",
      revokePairing: false,
      runtimeMembership: "untouched",
    });
  });

  it("does not turn a partial Cluster result into verified zero", () => {
    expect(
      incompleteTeardownOutcome("verified", { clusterTeardown }),
    ).toEqual({
      rustMustRevokePairing: false,
      runtimeMembership: "unknown",
      clusterTeardown,
    });
    expect(teardownOutcome("verified", false)).toEqual({
      rustMustRevokePairing: false,
      runtimeMembership: "verified_zero",
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
  });

  it("requires the current teardown target shape", () => {
    const current = {
      environments: [],
      destroyRuntimeProjects: false,
      revokePairing: false,
      runtimeMembership: "untouched",
    };
    expect(parseTeardownTargets(current)).toEqual(current);
    expect(() =>
      parseTeardownTargets({
        environments: [],
        revokePairing: false,
      }),
    ).toThrow();
    expect(() =>
      parseTeardownTargets({
        ...current,
        machines: [],
      }),
    ).toThrow();
  });

});
