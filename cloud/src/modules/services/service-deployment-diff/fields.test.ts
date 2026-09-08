import { decodeStrict } from "#/modules/environment-design/schema";
import { describe, expect, it } from "vitest";
import {
  serviceDeploymentConfigSchema,
  type ServiceDeployMount,
} from "#/modules/environment-design/services";
import {
  getManagedHostnameDriftRow,
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

describe("getManagedHostnameDriftRow", () => {
  const base = {
    serviceId: "service-1",
    managedHostname: { prefix: "api", targetPort: null },
    autoDomain: "new-lease.up.ployz.app",
  };

  it("flags a hostname served under an older cluster domain", () => {
    const row = getManagedHostnameDriftRow({
      ...base,
      boundHostnames: ["api.old-lease.up.ployz.app"],
    });
    expect(row).toMatchObject({
      path: "managedHostname.drift",
      currentValue: "api.old-lease.up.ployz.app",
      newValue: "api.new-lease.up.ployz.app",
    });
  });

  it("returns null when already serving the current domain", () => {
    expect(
      getManagedHostnameDriftRow({
        ...base,
        boundHostnames: ["api.new-lease.up.ployz.app"],
      }),
    ).toBeNull();
  });

  it("does not treat a staged prefix change as cluster-domain drift", () => {
    expect(
      getManagedHostnameDriftRow({
        ...base,
        managedHostname: { prefix: "web", targetPort: null },
        boundHostnames: ["api.new-lease.up.ployz.app"],
      }),
    ).toBeNull();
  });

  it("returns null without a managed hostname or auto domain", () => {
    expect(
      getManagedHostnameDriftRow({
        ...base,
        managedHostname: null,
        boundHostnames: ["api.old-lease.up.ployz.app"],
      }),
    ).toBeNull();
    expect(
      getManagedHostnameDriftRow({
        ...base,
        autoDomain: null,
        boundHostnames: ["api.old-lease.up.ployz.app"],
      }),
    ).toBeNull();
  });
});
