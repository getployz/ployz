import { describe, expect, it } from "vitest";
import { clampSince, retentionCutoff } from "#/modules/uptime/retention";

const organizationId = "00000000-0000-4000-8000-0000000c0001";
const now = new Date("2026-10-11T10:05:00.000Z");
const at = (iso: string) => new Date(iso);

describe("clampSince", () => {
  it("moves a raw read that reaches past 30 days up to exactly 30 days ago", () => {
    expect(clampSince(organizationId, "raw", at("2026-01-01T00:00:00.000Z"), now)).toEqual(at("2026-09-11T10:05:00.000Z"));
  });

  it("keeps a since that is at or after the cutoff", () => {
    expect(clampSince(organizationId, "raw", at("2026-09-11T10:05:00.000Z"), now)).toEqual(at("2026-09-11T10:05:00.000Z"));
    expect(clampSince(organizationId, "raw", at("2026-10-10T10:05:00.000Z"), now)).toEqual(at("2026-10-10T10:05:00.000Z"));
  });

  it("moves a since 1 ms before the cutoff up to the cutoff", () => {
    expect(clampSince(organizationId, "raw", at("2026-09-11T10:04:59.999Z"), now)).toEqual(at("2026-09-11T10:05:00.000Z"));
  });

  it("lets hourly and incident reads reach 13 months back, which raw can't", () => {
    const since = at("2026-01-01T00:00:00.000Z");
    expect(clampSince(organizationId, "hourly", since, now)).toEqual(since);
    expect(clampSince(organizationId, "incidents", since, now)).toEqual(since);
    expect(clampSince(organizationId, "hourly", at("2020-01-01T00:00:00.000Z"), now)).toEqual(at("2025-09-11T10:05:00.000Z"));
    expect(clampSince(organizationId, "incidents", at("2020-01-01T00:00:00.000Z"), now)).toEqual(at("2025-09-11T10:05:00.000Z"));
  });
});

describe("retentionCutoff", () => {
  it("clamps a month-end day to the shorter month 13 months back", () => {
    expect(retentionCutoff("hourly", at("2026-03-31T12:00:00.000Z"))).toEqual(at("2025-02-28T12:00:00.000Z"));
    expect(retentionCutoff("hourly", at("2025-03-31T12:00:00.000Z"))).toEqual(at("2024-02-29T12:00:00.000Z"));
  });

  it("keeps finished alerts 30 days", () => {
    expect(retentionCutoff("finishedAlerts", now)).toEqual(at("2026-09-11T10:05:00.000Z"));
  });
});
