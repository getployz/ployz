import { describe, expect, it } from "vitest";
import {
  GITHUB_CHECK_SUITE_ACTIONS as SCHEMA_ACTIONS,
  GITHUB_CHECK_SUITE_CONCLUSIONS as SCHEMA_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES as SCHEMA_STATUSES,
} from "#/db/schema";
import {
  GITHUB_CHECK_SUITE_ACTIONS as CONTRACT_ACTIONS,
  GITHUB_CHECK_SUITE_CONCLUSIONS as CONTRACT_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES as CONTRACT_STATUSES,
} from "#/modules/github/github-ingestion.contracts";
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

    expect(SCHEMA_ACTIONS).toBe(GITHUB_CHECK_SUITE_ACTIONS);
    expect(SCHEMA_STATUSES).toBe(GITHUB_CHECK_SUITE_STATUSES);
    expect(SCHEMA_CONCLUSIONS).toBe(GITHUB_CHECK_SUITE_CONCLUSIONS);
    expect(CONTRACT_ACTIONS).toBe(GITHUB_CHECK_SUITE_ACTIONS);
    expect(CONTRACT_STATUSES).toBe(GITHUB_CHECK_SUITE_STATUSES);
    expect(CONTRACT_CONCLUSIONS).toBe(GITHUB_CHECK_SUITE_CONCLUSIONS);
  });
});
