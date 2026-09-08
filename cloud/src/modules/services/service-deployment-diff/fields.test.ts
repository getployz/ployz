import { decodeStrict } from "#/modules/environment-design/schema";
import { describe, expect, it } from "vitest";
import {
  serviceDeploymentConfigSchema,
  type ServiceDeployMount,
} from "#/modules/environment-design/services";
import {
  getServiceDeploymentDiffRows,
} from "#/modules/services/service-deployment-diff/fields";

function config(mounts: ServiceDeployMount[]) {
  return decodeStrict(serviceDeploymentConfigSchema, {
    version: 2,
    name: "web",
    privateDns: "web",
    source: { version: 1, type: "empty", rootDir: "/" },
    preDeployCommand: null,
    startCommand: null,
    healthcheck: { type: "none" },
    restartPolicy: "unless-stopped",
    env: {},
    mounts,
  });
}

const volumeId = "11111111-1111-4111-8111-111111111111";

function mount(overrides: Partial<ServiceDeployMount> = {}): ServiceDeployMount {
  return {
    volumeResourceId: volumeId,
    volumeName: "shared-data",
    mountPath: "/data",
    ...overrides,
  };
}

function mountRows(current: ServiceDeployMount[], baseline: ServiceDeployMount[]) {
  return getServiceDeploymentDiffRows({
    serviceId: "service-1",
    current: config(current),
    baseline: config(baseline),
  }).filter((row) => row.path.startsWith("mounts."));
}

describe("service deployment mount diff", () => {
  it("reports an added mount as one owned service row", () => {
    const rows = mountRows([mount()], []);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      kind: "add",
      path: `mounts.${volumeId}`,
      newValue: "/data",
      currentValue: "",
      canDiscard: false,
    });
    expect(rows[0]?.derivedFrom).toBeUndefined();
  });

});
