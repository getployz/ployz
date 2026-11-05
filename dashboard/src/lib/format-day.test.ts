import { expect, it, vi } from "vitest";

it("formats the UTC day in en-US whatever the runtime's time zone", async () => {
  // Set before the import: the formatter is built when the module loads.
  vi.stubEnv("TZ", "Pacific/Kiritimati");
  vi.resetModules();
  try {
    const { formatDay } = await import("./format-day");
    expect(formatDay("2026-11-05T23:30:00Z")).toBe("Nov 5, 2026");
    expect(formatDay(new Date("2026-11-05T00:30:00Z"))).toBe("Nov 5, 2026");
  } finally {
    vi.unstubAllEnvs();
  }
});
