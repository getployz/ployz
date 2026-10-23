import { Schema } from "effect";
import { describe, expect, it } from "vitest";
import { RecoveryBackup } from "#/modules/machines/sealing-key";
import { recoveryDigest } from "#/modules/machines/sealing-key.server";

// The Engine's backup_format_stays_readable pins this vector and digest.
const ENGINE_BACKUP =
  "AQEBAQEBAQEBAQEBAQEBAQECAgICAgICAgICAgKoJ0JJzktDDEHUbyILrSrXrKnTNTVr5QvXa0O6Tvl0z02w36IqI7Bc1vhSDy0Yud8=";

describe("RecoveryBackup", () => {
  it("accepts the backup the Engine writes and digests it as the Engine does", () => {
    expect(Schema.decodeUnknownSync(RecoveryBackup)(ENGINE_BACKUP)).toBe(ENGINE_BACKUP);
    expect(recoveryDigest(ENGINE_BACKUP)).toBe("4ef804f83a444658d5336274cc7ad9fe8af91a47b0d3bfb903e98965439926e2");
  });
});
