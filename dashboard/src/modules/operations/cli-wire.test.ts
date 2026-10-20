import { Schema } from "effect";
import { describe, expect, it } from "vitest";
import { NewVolumeRun, volumeRunInput } from "#/modules/operations/cli-wire";

const environment = { project: "shop", environment: "production" };
const asked = (body: object) => volumeRunInput(Schema.decodeUnknownSync(NewVolumeRun)(body));

describe("volumeRunInput", () => {
  it("admits mirror rm --if-old-copy for an old copy only, and a bare mirror rm for either role", () => {
    expect(asked({ environment, kind: "delete_mirror", slot: "web-1", role: "old_copy" }))
      .toEqual({ kind: "delete_mirror", args: { slot: "web-1", confirmed_name: null, role: "old_copy" } });
    expect(asked({ environment, kind: "delete_mirror", slot: "web-1", confirm: "data" }))
      .toEqual({ kind: "delete_mirror", args: { slot: "web-1", confirmed_name: "data", role: "any" } });
  });

  it("refuses a role mirror rm cannot ask for", () => {
    expect(() => asked({ environment, kind: "delete_mirror", slot: "web-1", role: "mirror" })).toThrow();
  });
});
