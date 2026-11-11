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

  it("carries a Device's approval on a mirror, a full sync and a resize, and none when the CLI sent none", () => {
    const approval = { body: '{"volume":"v"}', signatures: [{ key: "a".repeat(64), signature: "b".repeat(128) }] };
    expect(volumeRunInput(decode({ environment, kind: "mirror", to: "web-2", approval })))
      .toEqual({ kind: "mirror", args: { to: "web-2", approval } });
    expect(volumeRunInput(decode({ environment, kind: "sync", full: true, approval })))
      .toEqual({ kind: "sync", args: { full: true, approval } });
    expect(volumeRunInput(decode({ environment, kind: "mirror", to: "web-2" }))).toEqual({ kind: "mirror", args: { to: "web-2" } });
    expect(volumeRunInput(decode({ environment, kind: "resize", maximum_bytes: 20_000_000_000, approval })))
      .toEqual({ kind: "resize", args: { maximum_bytes: 20_000_000_000, approval } });
    expect(volumeRunInput(decode({ environment, kind: "resize", maximum_bytes: 20_000_000_000 })))
      .toEqual({ kind: "resize", args: { maximum_bytes: 20_000_000_000 } });
  });

  it("refuses an approval that isn't shaped as a signed one", () => {
    expect(() => decode({ environment, kind: "mirror", to: "web-2", approval: { body: "{}", signatures: [{ key: "web-1", signature: "x" }] } })).toThrow();
    expect(() => decode({ environment, kind: "mirror", to: "web-2", approval: "signed" })).toThrow();
  });

  it("never reads a restriction from a field a bare mirror rm body carries", () => {
    expect(volumeRunInput(decode({ environment, kind: "delete_mirror", slot: "web-1", role: "old_copy" })))
      .toEqual({ kind: "delete_mirror", args: { slot: "web-1", confirmed_name: null, role: "any" } });
  });
});
