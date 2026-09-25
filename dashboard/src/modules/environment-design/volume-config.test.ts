import { describe, expect, it } from "vitest";
import { getVolumePhysicalName } from "#/modules/environment-design/volume-config";

describe("getVolumePhysicalName", () => {
  it("derives a stable name from the resource id (not the display name)", () => {
    expect(getVolumePhysicalName("abc")).toBe("vol-abc");
  });
});
