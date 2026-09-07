import { describe, expect, it } from "vitest";
import {
  createDefaultServiceHealthcheck,
  createDefaultServiceRestartPolicy,
  createGitServiceSource,
  createImageServiceSource,
  projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import { discardServiceDeploymentDiffPath } from "#/modules/services/service-deployment-diff/mutations";
import {
  getServiceDeploymentAttemptDiffRows,
  getServiceDeploymentDiffState,
} from "#/modules/services/service-deployment-diff/state";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";

const currentConfig = projectServiceDeploymentConfig({
  name: "api",
  privateDns: "api",
  source: createGitServiceSource({
    repository: "acme/api",
    repositoryId: 42,
    installationId: 7,
    rootDir: "/apps/api",
    branch: { type: "connected", name: "main" },
    waitForCi: false,
  }),
  preDeployCommand: null,
  startCommand: null,
  healthcheck: createDefaultServiceHealthcheck(),
  restartPolicy: createDefaultServiceRestartPolicy(),
});

describe("service deployment state", () => {
  it("applies a field discard to the explicit baseline", () => {
    const draft = {
      name: "api-next",
      source: createGitServiceSource({
        repository: "acme/api",
        repositoryId: 42,
        installationId: 7,
        rootDir: "/apps/api",
        branch: { type: "connected", name: "develop" },
        waitForCi: true,
      }),
      preDeployCommand: "npm run migrate",
      startCommand: "npm start",
      healthcheck: { type: "http" as const, path: "/health", timeoutSeconds: 15 },
      restartPolicy: "always" as const,
      privateDns: "api-next",
      deletedAt: new Date(),
    };

    discardServiceDeploymentDiffPath({
      draft,
      baseline: currentConfig,
      path: "source.branch",
    });
    expect(draft.source.type === "git" ? draft.source.branch : null).toEqual({
      type: "connected",
      name: "main",
    });
  });

  it("builds field state without false positives from object key order", () => {
    const baselineSource = createGitServiceSource({
      repository: "acme/api",
      repositoryId: 42,
      installationId: 7,
      rootDir: "/apps/api",
      branch: { type: "connected", name: "main" },
    });
    if (baselineSource.type !== "git") throw new Error("Expected git source");
    baselineSource.branch = { name: "main", type: "connected" };

    const diff = getServiceDeploymentDiffState({
      service: {
        ...currentConfig,
        id: crypto.randomUUID(),
        source: createGitServiceSource({
          repository: "acme/api",
          repositoryId: 42,
          installationId: 7,
          rootDir: "/apps/api",
          branch: { type: "connected", name: "main" },
        }),
      },
      comparison: {
        role: "saved",
        value: { ...currentConfig, source: baselineSource },
      },
    });

    expect(diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.sourceBranch)).toEqual({
      changed: false,
    });
  });

  it("diffs an Attempt Target against Applied config", () => {
    const deployed = projectServiceDeploymentConfig({
      ...currentConfig,
      source: createImageServiceSource({ image: "nginx:1.27" }),
    });
    const target = projectServiceDeploymentConfig({
      ...currentConfig,
      source: createImageServiceSource({ image: "nginx:1.28" }),
    });

    expect(
      getServiceDeploymentAttemptDiffRows({
        serviceId: "service-1",
        deployed,
        target,
      }),
    ).toEqual([
      expect.objectContaining({
        label: "Container image",
        currentValue: "nginx:1.27",
        newValue: "nginx:1.28",
      }),
    ]);
  });
});
