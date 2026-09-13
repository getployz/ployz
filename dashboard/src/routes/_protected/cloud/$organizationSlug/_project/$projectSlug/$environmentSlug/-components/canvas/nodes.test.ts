import { describe, expect, it } from "vitest";
import { buildEdges, buildNodes } from "./nodes";
import type {
  VariableGroupResourceRecord,
  VolumeResourceRecord,
} from "#/modules/environment-design/resources";
import type { ServiceCanvasPositionRecord } from "#/modules/environment-design/services";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import type { VariableRecord } from "#/modules/environment-design/variables";

function createVolumeRecord(
  overrides?: Partial<VolumeResourceRecord>,
): VolumeResourceRecord {
  const now = new Date("2026-06-04T00:00:00.000Z");
  return {
    resource: {
      id: "volume-1",
      projectId: "project-id",
      environmentId: "env-1",
      lineageId: "volume-lineage-1",
      implementationType: "volume",
      variableGroupId: null,
      name: "shared-data",
      slug: "shared-data",
      deletedAt: null,
      createdAt: now,
      updatedAt: now,
    },
    lineage: {
      id: "volume-lineage-1",
      projectId: "project-id",
      canonicalName: "shared-data",
      canonicalSlug: "shared-data",
      createdAt: now,
      updatedAt: now,
    },
    canvasPosition: null,
    attachments: [],
    consumerCount: 0,
    isAuthored: true,
    runtimeStatus: null,
    projectSlug: "project",
    environmentSlug: "production",
    ...overrides,
  };
}

function createServiceRecord(
  overrides?: Partial<EnvironmentServiceViewRecord>
): EnvironmentServiceViewRecord {
  return {
    service: {
      id: "service-1",
      environmentId: "env-1",
      name: "api",
      slug: "api",
      source: {
        version: 2,
        type: "git",
        repository: "acme/api",
        repositoryId: 42,
        installationId: 7,
        rootDir: ".",
        autoDeploy: true,
        waitForCi: false,
        branch: {
          type: "connected",
          name: "main",
        },
      },
      registryCredentialUsername: null,
      preDeployCommand: null,
      startCommand: null,
      healthcheck: {
        type: "http",
        path: "/healthz",
        timeoutSeconds: 5,
      },
      restartPolicy: "on-failure",
      createdAt: new Date("2026-03-26T00:00:00.000Z"),
      updatedAt: new Date("2026-03-26T00:00:00.000Z"),
      projectSlug: "project",
      environmentSlug: "production",
      $synced: true,
      $origin: "remote",
      $key: "service-1",
      $collectionId: "services",
    },
    canvasPositions: [
      {
        id: "pos-1",
        environmentId: "env-1",
        resourceType: "service",
        resourceId: "service-1",
        x: 400,
        y: 280,
        createdAt: new Date("2026-03-26T00:00:00.000Z"),
        updatedAt: new Date("2026-03-26T00:00:00.000Z"),
        $synced: true,
        $origin: "remote",
        $key: "service-1",
        $collectionId: "service-positions",
      },
    ],
    variables: [],
    ...overrides,
  } as EnvironmentServiceViewRecord;
}

function createPlainVariable(value: string): VariableRecord {
  return {
    id: "var-1",
    key: "DATABASE_URL",
    value: { type: "plain", value },
  } as VariableRecord;
}

function createVariableGroupRecord(
  overrides?: Partial<VariableGroupResourceRecord>,
): VariableGroupResourceRecord {
  const now = new Date("2026-06-03T00:00:00.000Z");

  return {
    resource: {
      id: "resource-1",
      projectId: "project-id",
      environmentId: "env-1",
      lineageId: "resource-lineage-1",
      implementationType: "variable_group",
      variableGroupId: "variable-group-1",
      name: "Database",
      slug: "database",
      deletedAt: null,
      createdAt: now,
      updatedAt: now,
    },
    lineage: {
      id: "resource-lineage-1",
      projectId: "project-id",
      canonicalName: "Database",
      canonicalSlug: "database-resource",
      createdAt: now,
      updatedAt: now,
    },
    variableGroup: {
      id: "variable-group-1",
      projectId: "project-id",
      environmentId: "env-1",
      lineageId: "variable-group-lineage-1",
      name: "Database",
      slug: "database",
      createdAt: now,
      updatedAt: now,
    },
    canvasPosition: {
      id: "resource-position-1",
      environmentId: "env-1",
      resourceType: "variable_group",
      resourceId: "resource-1",
      x: 80,
      y: 120,
      createdAt: now,
      updatedAt: now,
    },
    variables: [],
    exports: [],
    consumerCount: 0,
    projectSlug: "project",
    environmentSlug: "production",
    ...overrides,
  };
}

function createCanvasPosition(
  overrides?: Partial<ServiceCanvasPositionRecord>,
): ServiceCanvasPositionRecord {
  return {
    id: "pos-1",
    environmentId: "env-1",
    resourceType: "service",
    resourceId: "service-1",
    x: 400,
    y: 280,
    createdAt: new Date("2026-03-26T00:00:00.000Z"),
    updatedAt: new Date("2026-03-26T00:00:00.000Z"),
    ...overrides,
  };
}

describe("buildNodes", () => {
  it("leaves every node unselected when no inspector is open", () => {
    const nodes = buildNodes(
      [createServiceRecord()],
      [createVariableGroupRecord()],
      [],
      null,
      [createVolumeRecord()],
    );
    for (const node of nodes) {
      expect(node.selected).toBe(false);
      expect(node.selectable).not.toBe(true);
    }
  });

  it("uses the collection position", () => {
    const [node] = buildNodes(
      [createServiceRecord()],
      [],
      [createCanvasPosition()],
      null,
    );

    expect(node?.position).toEqual({ x: 400, y: 280 });
  });

  it("marks the selected service", () => {
    const [node] = buildNodes(
      [createServiceRecord()],
      [],
      [createCanvasPosition()],
      "service-1",
    );

    expect(node?.selected).toBe(true);
  });

  it("adds Variable Group resources as canvas nodes", () => {
    const nodes = buildNodes(
      [],
      [createVariableGroupRecord()],
      [
        createCanvasPosition({
          id: "resource-position-1",
          resourceType: "variable_group",
          resourceId: "resource-1",
          x: 80,
          y: 120,
        }),
      ],
      null,
    );

    expect(nodes).toEqual([
      expect.objectContaining({
        id: "resource-1",
        type: "variable_group",
        position: { x: 80, y: 120 },
        data: {
          resourceType: "variable_group",
          resourceId: "resource-1",
          environmentId: "env-1",
        },
      }),
    ]);
  });

  it("adds Volume resources as canvas nodes", () => {
    const nodes = buildNodes(
      [],
      [],
      [
        createCanvasPosition({
          id: "volume-position-1",
          resourceType: "volume",
          resourceId: "volume-1",
          x: 40,
          y: 60,
        }),
      ],
      null,
      [createVolumeRecord()],
    );

    expect(nodes).toEqual([
      expect.objectContaining({
        id: "volume-1",
        type: "volume",
        position: { x: 40, y: 60 },
        data: {
          resourceType: "volume",
          resourceId: "volume-1",
          environmentId: "env-1",
        },
      }),
    ]);
  });

  it("builds service-volume mount edges", () => {
    const edges = buildEdges(
      [],
      [],
      [createVolumeRecord()],
      [
        {
          environmentId: "env-1",
          serviceId: "service-1",
          volumeResourceId: "volume-1",
          mountPath: "/data",
        },
      ],
    );

    expect(edges).toEqual([
      expect.objectContaining({
        id: "mount:volume-1:service-1",
        source: "volume-1",
        target: "service-1",
      }),
    ]);
  });

  it("omits mount edges for a volume removed from the authored document", () => {
    const edges = buildEdges(
      [],
      [],
      [createVolumeRecord({ isAuthored: false })],
      [
        {
          environmentId: "env-1",
          serviceId: "service-1",
          volumeResourceId: "volume-1",
          mountPath: "/data",
        },
      ],
    );

    expect(edges).toEqual([]);
  });

  it("builds collapsed Variable Group attachment edges", () => {
    const edges = buildEdges(
      [createVariableGroupRecord()],
      [
        {
          environmentId: "env-1",
          serviceId: "service-1",
          variableGroupId: "variable-group-1",
          sortOrder: 0,
        },
      ],
    );

    expect(edges).toEqual([
      expect.objectContaining({
        id: "attachment:resource-1:service-1",
        // Variable Group (exit/top) -> service (entry/bottom): arrow points at
        // the consuming service, and the group renders below it (like volumes).
        source: "resource-1",
        target: "service-1",
      }),
    ]);
  });

  it("builds reference edges from a ${{ }} template ref to its producer", () => {
    const edges = buildEdges(
      [createVariableGroupRecord()],
      [],
      [],
      [],
      [
        createServiceRecord({
          variables: [createPlainVariable("${{ database.URL }}")],
        }),
      ],
    );

    expect(edges).toEqual([
      expect.objectContaining({
        id: "reference:resource-1:service-1",
        // Producer (Variable Group) -> consumer (service): arrow points at the
        // service.
        source: "resource-1",
        target: "service-1",
      }),
    ]);
  });

  it("does not duplicate a reference edge when an attachment already links the pair", () => {
    const edges = buildEdges(
      [createVariableGroupRecord()],
      [
        {
          environmentId: "env-1",
          serviceId: "service-1",
          variableGroupId: "variable-group-1",
          sortOrder: 0,
        },
      ],
      [],
      [],
      [
        createServiceRecord({
          variables: [createPlainVariable("${{ database.URL }}")],
        }),
      ],
    );

    expect(edges).toEqual([
      expect.objectContaining({ id: "attachment:resource-1:service-1" }),
    ]);
  });

  it("ignores self references (no owner slug)", () => {
    const edges = buildEdges(
      [],
      [],
      [],
      [],
      [
        createServiceRecord({
          variables: [createPlainVariable("${{ SELF_KEY }}")],
        }),
      ],
    );

    expect(edges).toEqual([]);
  });
});
