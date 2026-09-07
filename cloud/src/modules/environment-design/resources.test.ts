import {
  decodeStrict,
  isValid,
} from "#/modules/environment-design/schema";
import { describe, expect, it } from "vitest";
import {
  createVolumeResourceSchema,
  createVariableGroupResourceSchema,
  variableGroupResourceRecordSchema,
  volumeResourceRecordSchema,
} from "#/modules/environment-design/resources";

const now = new Date("2026-06-03T00:00:00.000Z");

function volumeResourceRecord() {
  return {
    resource: {
      id: "11111111-1111-4111-8111-111111111111",
      projectId: "22222222-2222-4222-8222-222222222222",
      environmentId: "33333333-3333-4333-8333-333333333333",
      lineageId: "44444444-4444-4444-8444-444444444444",
      implementationType: "volume" as const,
      variableGroupId: null,
      name: "shared-data",
      slug: "shared-data",
      deletedAt: null,
      createdAt: now,
      updatedAt: now,
    },
    lineage: {
      id: "44444444-4444-4444-8444-444444444444",
      projectId: "22222222-2222-4222-8222-222222222222",
      canonicalName: "shared-data",
      canonicalSlug: "shared-data-44444444",
      createdAt: now,
      updatedAt: now,
    },
    canvasPosition: null,
    attachments: [],
    consumerCount: 0,
    runtimeStatus: null,
    projectSlug: "project",
    environmentSlug: "production",
  };
}

function variableGroupResourceRecord() {
  return {
    resource: {
      id: "11111111-1111-4111-8111-111111111111",
      projectId: "22222222-2222-4222-8222-222222222222",
      environmentId: "33333333-3333-4333-8333-333333333333",
      lineageId: "44444444-4444-4444-8444-444444444444",
      implementationType: "variable_group" as const,
      variableGroupId: "55555555-5555-4555-8555-555555555555",
      name: "Database",
      slug: "database",
      deletedAt: null,
      createdAt: now,
      updatedAt: now,
    },
    lineage: {
      id: "44444444-4444-4444-8444-444444444444",
      projectId: "22222222-2222-4222-8222-222222222222",
      canonicalName: "Database",
      canonicalSlug: "database-44444444",
      createdAt: now,
      updatedAt: now,
    },
    variableGroup: {
      id: "55555555-5555-4555-8555-555555555555",
      projectId: "22222222-2222-4222-8222-222222222222",
      environmentId: "33333333-3333-4333-8333-333333333333",
      lineageId: "66666666-6666-4666-8666-666666666666",
      name: "Database",
      slug: "database",
      createdAt: now,
      updatedAt: now,
    },
    canvasPosition: {
      id: "77777777-7777-4777-8777-777777777777",
      environmentId: "33333333-3333-4333-8333-333333333333",
      resourceType: "variable_group",
      resourceId: "11111111-1111-4111-8111-111111111111",
      x: 120,
      y: 240,
      createdAt: now,
      updatedAt: now,
    },
    variables: [
      {
        id: "88888888-8888-4888-8888-888888888888",
        serviceId: null,
        variableGroupId: "55555555-5555-4555-8555-555555555555",
        configKeyId: "99999999-9999-4999-8999-999999999999",
        key: "DATABASE_URL",
        description: null,
        exported: true,
        value: {
          type: "sealed" as const,
          hasValue: true as const,
          fingerprint: "secret:fingerprint",
        },
        createdAt: now,
        updatedAt: now,
      },
    ],
    exports: [
      {
        key: "DATABASE_URL",
        value: {
          type: "sealed" as const,
          hasValue: true as const,
          fingerprint: "secret:fingerprint",
        },
        variableId: "88888888-8888-4888-8888-888888888888",
      },
    ],
    consumerCount: 0,
    projectSlug: "project",
    environmentSlug: "production",
  };
}

describe("environment resource models", () => {
  it("defaults Variable Group creation coordinates and trims the resource name", () => {
    const parsed = decodeStrict(createVariableGroupResourceSchema, {
      organizationSlug: "acme",
      environmentId: "33333333-3333-4333-8333-333333333333",
      name: " Database ",
    });

    expect(parsed).toMatchObject({
      name: "Database",
      x: 0,
      y: 0,
    });
  });

  it("models Variable Group resources separately from their backing variable group", () => {
    const parsed = decodeStrict(variableGroupResourceRecordSchema, variableGroupResourceRecord());

    expect(parsed.resource.lineageId).not.toBe(parsed.variableGroup.lineageId);
    expect(parsed.resource.variableGroupId).toBe(parsed.variableGroup.id);
    expect(parsed.exports).toHaveLength(1);
    expect(parsed.exports[0]).toMatchObject({
      key: "DATABASE_URL",
      variableId: "88888888-8888-4888-8888-888888888888",
    });
  });

  it("models a volume resource with no backing variable group", () => {
    const parsed = decodeStrict(volumeResourceRecordSchema, volumeResourceRecord());

    expect(parsed.resource.implementationType).toBe("volume");
    expect(parsed.resource.variableGroupId).toBeNull();
    expect(parsed.resource.deletedAt).toBeNull();
    expect(parsed.attachments).toEqual([]);
    expect(parsed.consumerCount).toBe(0);
    expect(parsed.runtimeStatus).toBeNull();
  });

  it("models a volume resource mounted on multiple services", () => {
    const parsed = decodeStrict(volumeResourceRecordSchema, {
      ...volumeResourceRecord(),
      attachments: [
        { serviceId: "77777777-7777-4777-8777-777777777777", mountPath: "/data" },
        { serviceId: "88888888-8888-4888-8888-888888888888", mountPath: "/cache" },
      ],
      consumerCount: 2,
    });

    expect(parsed.attachments).toHaveLength(2);
    expect(parsed.consumerCount).toBe(2);
  });

  it("rejects extra storage fields on volume creation", () => {
    const parsed = decodeStrict(createVolumeResourceSchema, {
      organizationSlug: "acme",
      environmentId: "33333333-3333-4333-8333-333333333333",
      name: "Data",
    });

    expect(parsed).toMatchObject({
      name: "Data",
      x: 0,
      y: 0,
    });
    expect(
      isValid(createVolumeResourceSchema, {
        organizationSlug: "acme",
        environmentId: "33333333-3333-4333-8333-333333333333",
        name: "Data",
        storage: { kind: "plain" },
      }),
    ).toBe(false);
  });

  it("rejects an Variable Group with no variable group", () => {
    const record = variableGroupResourceRecord();
    const result = isValid(variableGroupResourceRecordSchema, {
      ...record,
      resource: { ...record.resource, variableGroupId: null },
    });

    expect(result).toBe(false);
  });
});
