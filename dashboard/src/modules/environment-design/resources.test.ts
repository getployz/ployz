import {
  decodeStrict,
  isValid,
} from "#/modules/environment-design/schema";
import { describe, expect, it } from "vitest";
import {
  createVolumeResourceSchema,
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
    isAuthored: true,
    runtimeStatus: null,
    projectSlug: "project",
    environmentSlug: "production",
  };
}

describe("environment resource models", () => {
  it("models an authored volume resource with no mounts", () => {
    const parsed = decodeStrict(volumeResourceRecordSchema, volumeResourceRecord());

    expect(parsed.resource.implementationType).toBe("volume");
    expect(parsed.resource.deletedAt).toBeNull();
    expect(parsed.attachments).toEqual([]);
    expect(parsed.consumerCount).toBe(0);
    expect(parsed.isAuthored).toBe(true);
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
});
