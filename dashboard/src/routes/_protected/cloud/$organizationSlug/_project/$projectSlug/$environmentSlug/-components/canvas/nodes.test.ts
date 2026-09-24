import { describe, expect, it } from "vitest";
import { buildEdges, buildNodes } from "./nodes";
import type { VolumeResourceRecord } from "#/modules/environment-design/resources";
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

function createDatabaseService(): EnvironmentServiceViewRecord {
  const service = createServiceRecord();
  return { ...service, service: { ...service.service, id: "service-db", name: "db", slug: "db" } };
}

describe("buildNodes", () => {
  it("leaves every node unselected when no inspector is open", () => {
    const nodes = buildNodes(
      [createServiceRecord()],
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
      [createCanvasPosition()],
      null,
    );

    expect(node?.position).toEqual({ x: 400, y: 280 });
  });

  it("marks the selected service", () => {
    const [node] = buildNodes(
      [createServiceRecord()],
      [createCanvasPosition()],
      "service-1",
    );

    expect(node?.selected).toBe(true);
  });

  it("adds Volume resources as canvas nodes", () => {
    const nodes = buildNodes(
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

  it("builds one reference edge per ${{ }} producer service", () => {
    const edges = buildEdges([], [], [
      createDatabaseService(),
      createServiceRecord({
        variables: [
          createPlainVariable("${{ db.URL }}"),
          { ...createPlainVariable("${{ db.PASSWORD }}"), id: "var-2", key: "DATABASE_PASSWORD" },
        ],
      }),
    ]);

    expect(edges).toEqual([
      expect.objectContaining({
        id: "reference:service-db:service-1",
        // Producer -> consumer: arrow points at the consuming service.
        source: "service-db",
        target: "service-1",
      }),
    ]);
  });

  it("ignores self references (no owner slug)", () => {
    const edges = buildEdges(
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
