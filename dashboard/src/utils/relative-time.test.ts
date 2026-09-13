import { describe, expect, it } from "vitest";
import { formatRelativeTime } from "#/utils/relative-time";

describe("formatRelativeTime", () => {
  const now = new Date("2026-05-09T12:00:00.000Z");

  it("describes past times", () => {
    expect(
      formatRelativeTime(new Date("2026-05-09T11:04:00.000Z"), now),
    ).toBe("56 minutes ago");
  });

  it("falls back to hours and days", () => {
    expect(formatRelativeTime(new Date("2026-05-09T09:00:00.000Z"), now)).toBe(
      "3 hours ago",
    );
    expect(formatRelativeTime(new Date("2026-05-07T12:00:00.000Z"), now)).toBe(
      "2 days ago",
    );
  });
});
