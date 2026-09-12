import { describe, expect, it } from "vitest";
import { resolveEnvironmentWorkingComparison } from "#/modules/environment-design/environment-change-set";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import { getServiceDeploymentDiffState } from "#/modules/services/service-deployment-diff/state";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

function config(replicas: number): ServiceDeploymentConfig {
  return {
    version: 2,
    name: "api",
    source: {
      version: 1,
      type: "image",
      image: "docker.io/library/nginx:stable",
      autoUpdate: { type: "off" },
      credentials: { type: "none" },
    },
    preDeployCommand: null,
    startCommand: null,
    healthcheck: { type: "none" },
    restartPolicy: "unless-stopped",
    maxRetries: 10,
    cron: null,
    replicas,
    cpuLimit: null,
    memLimit: null,
    privateDns: "api",
    routes: [],
    managedHostname: null,
    build: { builder: "auto", dockerfilePath: null, watchPaths: [] },
    env: {},
    mounts: [],
    variableGroupAttachments: [],
  };
}

describe("service drawer Working comparison", () => {
  it.each([
    {
      baseline: config(2),
      introduction: config(0),
      baselineLabel: "Current",
      baselineValue: "2",
    },
    {
      baseline: null,
      introduction: config(1),
      baselineLabel: "Introduced",
      baselineValue: "1",
    },
  ])("preserves $baselineLabel provenance for field copy", (input) => {
    const working = config(3);
    const diff = getServiceDeploymentDiffState({
      service: { id: "service-1", ...working },
      comparison: resolveEnvironmentWorkingComparison({ baseline: input.baseline, introduction: input.introduction }),
    });

    expect(diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.replicas)).toMatchObject({
      changed: true,
      baselineLabel: input.baselineLabel,
      baselineValue: input.baselineValue,
      currentValue: "3",
    });
  });

  it("does not invent field comparisons without a baseline or Introduction", () => {
    const working = config(3);
    const diff = getServiceDeploymentDiffState({
      service: { id: "service-1", ...working },
      comparison: resolveEnvironmentWorkingComparison<ServiceDeploymentConfig>({
        baseline: null,
        introduction: null,
      }),
    });

    expect(diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.replicas)).toEqual({
      changed: false,
    });
  });
});
