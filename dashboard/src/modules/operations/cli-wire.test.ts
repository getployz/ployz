import { Schema } from "effect";
import { describe, expect, it } from "vitest";
import { NewVolumeRun, volumeRunInput } from "#/modules/operations/cli-wire";

const environment = { project: "shop", environment: "production" };
const decode = Schema.decodeUnknownSync(NewVolumeRun);

describe("volumeRunInput", () => {
  it("admits mirror rm --if-old-copy for an old copy only, and a bare mirror rm for either role", () => {
    expect(volumeRunInput(decode({ environment, kind: "delete_old_copy", slot: "web-1" })))
      .toEqual({ kind: "delete_mirror", args: { slot: "web-1", confirmed_name: null, role: "old_copy" } });
    expect(volumeRunInput(decode({ environment, kind: "delete_mirror", slot: "web-1", confirm: "data" })))
      .toEqual({ kind: "delete_mirror", args: { slot: "web-1", confirmed_name: "data", role: "any" } });
  });

  it("never reads a restriction from a field a bare mirror rm body carries", () => {
    expect(volumeRunInput(decode({ environment, kind: "delete_mirror", slot: "web-1", role: "old_copy" })))
      .toEqual({ kind: "delete_mirror", args: { slot: "web-1", confirmed_name: null, role: "any" } });
  });
});
