import { describe, expect, it } from "vitest";
import { requirements } from "#/modules/runtime/daemon-compat";
import {
  backupFromWire, dailyBackupId, endpointUrl, goneBackups, offsiteCopyOf, offsiteServed, regionOf, uploadBackupId,
} from "./volume-offsite";

const ALL = [requirements.offsiteSet, requirements.offsiteInspect, requirements.offsiteCheck].map((requirement) => requirement.capability);
const at = (iso: string) => new Date(iso);

describe("offsiteServed", () => {
  it("serves only a writer listing all three offsite capabilities", () => {
    expect(offsiteServed([{ id: "w", capabilities: ALL }], "w")).toBe(true);
    for (const missing of ALL) {
      expect(offsiteServed([{ id: "w", capabilities: ALL.filter((capability) => capability !== missing) }], "w")).toBe(false);
    }
  });

  it("hides offsite for a writer that lists no capabilities, an unknown writer, or no writer", () => {
    expect(offsiteServed([{ id: "w", capabilities: [] }], "w")).toBe(false);
    expect(offsiteServed([{ id: "w", capabilities: ALL }], "other")).toBe(false);
    expect(offsiteServed([{ id: "w", capabilities: ALL }], null)).toBe(false);
  });

  it("reads the capability on the writer, not on another Server", () => {
    expect(offsiteServed([{ id: "w", capabilities: [] }, { id: "m", capabilities: ALL }], "w")).toBe(false);
  });
});

describe("backup ids", () => {
  it("mints the day's id from the Volume and the tick, so a retried day names the same backup", () => {
    const tick = at("2026-10-12T00:00:00Z");
    expect(dailyBackupId("vol-1", tick)).toBe(dailyBackupId("vol-1", new Date(tick)));
    expect(dailyBackupId("vol-1", tick)).toMatch(/^off-1791763200-[0-9a-z]+$/);
    expect(dailyBackupId("vol-2", tick)).not.toBe(dailyBackupId("vol-1", tick));
    expect(dailyBackupId("vol-1", at("2026-10-13T00:00:00Z"))).not.toBe(dailyBackupId("vol-1", tick));
  });

  it("names one upload per source snapshot, apart from the daily ones", () => {
    expect(uploadBackupId("vol-1", "g1")).toBe(uploadBackupId("vol-1", "g1"));
    expect(uploadBackupId("vol-1", "g2")).not.toBe(uploadBackupId("vol-1", "g1"));
    expect(uploadBackupId("vol-1", "g1")).toMatch(/^up-/);
  });
});

describe("goneBackups", () => {
  const backups = [
    { id: "b", at: at("2026-10-10T00:00:00Z") },
    { id: "d", at: at("2026-10-12T00:00:00Z") },
    { id: "a", at: at("2026-10-09T00:00:00Z") },
    { id: "c", at: at("2026-10-11T00:00:00Z") },
  ];

  it("deletes the oldest past keep, whatever order the bucket lists them in", () => {
    expect(goneBackups(backups, 2)).toEqual(["b", "a"]);
    expect(goneBackups(backups, 4)).toEqual([]);
    expect(goneBackups(backups, 7)).toEqual([]);
  });

  it("always keeps the newest backup", () => {
    expect(goneBackups(backups, 0)).toEqual(["c", "b", "a"]);
  });
});

describe("bucket words", () => {
  it("turns a pasted host into an https endpoint and keeps an explicit scheme", () => {
    expect(endpointUrl(" s3.eu-central-1.amazonaws.com/ ")).toBe("https://s3.eu-central-1.amazonaws.com");
    expect(endpointUrl("http://minio.internal:9000")).toBe("http://minio.internal:9000");
  });

  it("reads the AWS region from the host and defaults elsewhere", () => {
    expect(regionOf("https://s3.eu-central-1.amazonaws.com")).toBe("eu-central-1");
    expect(regionOf("https://acme.s3.ap-southeast-2.amazonaws.com")).toBe("ap-southeast-2");
    expect(regionOf("https://fsn1.your-objectstorage.com")).toBe("us-east-1");
  });

  it("drops an empty note and dates a wire backup", () => {
    expect(backupFromWire({ id: "off-1", taken_unix_seconds: 1_791_763_200, bytes: 5, note: "" }))
      .toEqual({ id: "off-1", at: at("2026-10-12T00:00:00Z"), bytes: 5, note: null });
  });

  it("names the copy by its bucket and region, never a key", () => {
    const copy = offsiteCopyOf({ bucket: { id: "bk", bucket: "acme-offsite", region: "eu-central-1" }, keep: 7, uploadedAt: null }, "Uploading");
    expect(copy).toEqual({ kind: "offsite", id: "bk", bucket: "acme-offsite", region: "eu-central-1", keep: 7, uploadedAt: null, staged: null, busy: "Uploading" });
  });
});
