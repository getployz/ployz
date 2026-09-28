import { describe, expect, it } from "vitest";
import { reviveDurableAttemptDates } from "./durable-attempt-dates";

describe("reviveDurableAttemptDates", () => {
  const at = new Date("2026-09-28T01:45:52.222Z");

  it("revives a replayed step's JSON dates", () => {
    const revived = reviveDurableAttemptDates({
      id: "a", createdAt: at.toISOString(), startedAt: at.toISOString(), terminalAt: null, updatedAt: at.toISOString(),
    });
    expect(revived).toEqual({ id: "a", createdAt: at, startedAt: at, terminalAt: null, updatedAt: at });
  });

  it("takes a checkpointed step's Dates as they are", () => {
    const revived = reviveDurableAttemptDates({ id: "a", createdAt: at, startedAt: at, terminalAt: null, updatedAt: at });
    expect(revived).toEqual({ id: "a", createdAt: at, startedAt: at, terminalAt: null, updatedAt: at });
  });
});
