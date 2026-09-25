import { describe, expect, it } from "vitest";
import {
  GITHUB_CHECK_SUITE_ACTIONS,
  GITHUB_CHECK_SUITE_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES,
} from "#/modules/github/github-check-suite-vocabulary";

describe("GitHub check-suite vocabulary", () => {
  it("owns the exact accepted values shared by storage and contracts", () => {
    expect(GITHUB_CHECK_SUITE_ACTIONS).toEqual([
      "requested",
      "rerequested",
      "completed",
    ]);
    expect(GITHUB_CHECK_SUITE_STATUSES).toEqual([
      "queued",
      "in_progress",
      "completed",
      "pending",
      "waiting",
      "requested",
    ]);
    expect(GITHUB_CHECK_SUITE_CONCLUSIONS).toEqual([
      "action_required",
      "cancelled",
      "failure",
      "neutral",
      "success",
      "skipped",
      "stale",
      "timed_out",
      "startup_failure",
    ]);
  });
});
