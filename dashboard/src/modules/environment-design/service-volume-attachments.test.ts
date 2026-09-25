import { describe, expect, it } from "vitest";
import {
  decodeStrict,
  isValid,
} from "#/modules/environment-design/schema";
import {
  getMountConflict,
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
