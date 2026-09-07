import { describe, expect, it } from "vitest";
import {
  decodeStrict,
  isValid,
} from "#/modules/environment-design/schema";
import {
  getAttachmentTargetError,
  getMountConflict,
  getServiceMountsByServiceId,
  mountPathSchema,
  type ServiceMount,
} from "#/modules/environment-design/service-volume-attachments";

describe("mountPathSchema", () => {
  it("normalizes whitespace and a trailing slash", () => {
    expect(decodeStrict(mountPathSchema, "  /data/  ")).toBe("/data");
    expect(decodeStrict(mountPathSchema, "/var/lib/data")).toBe("/var/lib/data");
  });

  it("keeps the root path as /", () => {
    expect(decodeStrict(mountPathSchema, "/")).toBe("/");
  });

  it("rejects blank, relative, and doubled-slash paths", () => {
    expect(isValid(mountPathSchema, "")).toBe(false);
    expect(isValid(mountPathSchema, "   ")).toBe(false);
    expect(isValid(mountPathSchema, "data")).toBe(false);
    expect(isValid(mountPathSchema, "/a//b")).toBe(false);
  });
});

describe("getMountConflict", () => {
  const mounts: ServiceMount[] = [
    { volumeResourceId: "vol-a", mountPath: "/data" },
  ];

  it("blocks attaching the same volume twice", () => {
    expect(
      getMountConflict({
        serviceMounts: mounts,
        volumeResourceId: "vol-a",
        mountPath: "/elsewhere",
        mode: "attach",
      }),
    ).toEqual({ type: "duplicate_pair" });
  });

  it("blocks a second volume at an already-used path", () => {
    expect(
      getMountConflict({
        serviceMounts: mounts,
        volumeResourceId: "vol-b",
        mountPath: "/data",
        mode: "attach",
      }),
    ).toEqual({ type: "duplicate_mount_path", mountPath: "/data" });
  });

  it("allows a different volume at a different path", () => {
    expect(
      getMountConflict({
        serviceMounts: mounts,
        volumeResourceId: "vol-b",
        mountPath: "/cache",
        mode: "attach",
      }),
    ).toBeNull();
  });

  it("lets an existing mount keep its pair while editing its path", () => {
    expect(
      getMountConflict({
        serviceMounts: mounts,
        volumeResourceId: "vol-a",
        mountPath: "/cache",
        mode: "edit",
      }),
    ).toBeNull();
  });

  it("blocks an edit that collides with another volume's path", () => {
    expect(
      getMountConflict({
        serviceMounts: [
          { volumeResourceId: "vol-a", mountPath: "/data" },
          { volumeResourceId: "vol-b", mountPath: "/cache" },
        ],
        volumeResourceId: "vol-a",
        mountPath: "/cache",
        mode: "edit",
      }),
    ).toEqual({ type: "duplicate_mount_path", mountPath: "/cache" });
  });
});

describe("getAttachmentTargetError", () => {
  it("rejects cross-environment mounts", () => {
    expect(
      getAttachmentTargetError({
        serviceEnvironmentId: "env-1",
        resourceEnvironmentId: "env-2",
        resourceImplementationType: "volume",
      }),
    ).toBe("cross_environment");
  });

  it("rejects mounting a non-volume resource", () => {
    expect(
      getAttachmentTargetError({
        serviceEnvironmentId: "env-1",
        resourceEnvironmentId: "env-1",
        resourceImplementationType: "variable_group",
      }),
    ).toBe("non_volume_resource");
  });

  it("accepts a volume in the same environment", () => {
    expect(
      getAttachmentTargetError({
        serviceEnvironmentId: "env-1",
        resourceEnvironmentId: "env-1",
        resourceImplementationType: "volume",
      }),
    ).toBeNull();
  });
});

describe("getServiceMountsByServiceId", () => {
  const volumeA = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const volumeB = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

  it("groups mounts per service and resolves volume display names", () => {
    const byService = getServiceMountsByServiceId({
      attachments: [
        { serviceId: "web", volumeResourceId: volumeA, mountPath: "/data" },
        { serviceId: "worker", volumeResourceId: volumeA, mountPath: "/cache" },
      ],
      volumeNameById: new Map([[volumeA, "shared-data"]]),
    });

    expect(byService.get("web")).toEqual([
      { volumeResourceId: volumeA, volumeName: "shared-data", mountPath: "/data" },
    ]);
    expect(byService.get("worker")?.[0]?.mountPath).toBe("/cache");
  });

  it("sorts a service's mounts by path", () => {
    const byService = getServiceMountsByServiceId({
      attachments: [
        { serviceId: "web", volumeResourceId: volumeB, mountPath: "/z" },
        { serviceId: "web", volumeResourceId: volumeA, mountPath: "/a" },
      ],
      volumeNameById: new Map([
        [volumeA, "a-vol"],
        [volumeB, "z-vol"],
      ]),
    });

    expect(byService.get("web")?.map((mount) => mount.mountPath)).toEqual([
      "/a",
      "/z",
    ]);
  });

  it("drops mounts whose volume is absent (tombstoned) so the service sheds them", () => {
    const byService = getServiceMountsByServiceId({
      attachments: [
        { serviceId: "web", volumeResourceId: volumeA, mountPath: "/data" },
      ],
      volumeNameById: new Map(),
    });

    expect(byService.get("web")).toBeUndefined();
  });
});
