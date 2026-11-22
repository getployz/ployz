import { describe, expect, it } from "vitest";
import { createGithubCheckSuiteReceivedEvent, createGithubPushReceivedEvent } from "#/modules/github/github.events";

describe("GitHub events", () => {
  it("creates delivery-deduplicated GitHub ingestion events with authority keys", () => {
    expect(
      createGithubPushReceivedEvent({
        deliveryId: "delivery-push-1",
        kind: "push",
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        branch: "main",
        beforeSha: "A".repeat(40),
        afterSha: "B".repeat(40),
        created: false,
        deleted: false,
        forced: false,
      }),
    ).toEqual({
      id: "delivery-push-1",
      name: "github/push.received",
      data: expect.objectContaining({
        deliveryId: "delivery-push-1",
        branchKey: "17:42:refs/heads/main",
        beforeSha: "a".repeat(40),
        afterSha: "b".repeat(40),
      }),
    });
    expect(
      createGithubCheckSuiteReceivedEvent({
        deliveryId: "delivery-suite-1",
        kind: "check_suite",
        action: "completed",
        installationId: 17,
        repositoryId: 42,
        checkSuiteId: 9001,
        headSha: "C".repeat(40),
        status: "completed",
        conclusion: "success",
        sourceUpdatedAt: "2026-07-16T08:00:00.000+01:00",
      }),
    ).toEqual({
      id: "delivery-suite-1",
      name: "github/check-suite.received",
      data: expect.objectContaining({
        deliveryId: "delivery-suite-1",
        checkSuiteKey: "17:42:9001",
        headSha: "c".repeat(40),
        sourceUpdatedAt: "2026-07-16T07:00:00.000Z",
      }),
    });
  });

  it("rejects non-canonical GitHub event authority data", () => {
    expect(() =>
      createGithubPushReceivedEvent({
        deliveryId: "delivery-push-invalid",
        kind: "push",
        installationId: 17,
        repositoryId: 42,
        ref: "refs/tags/release",
        branch: "release",
        beforeSha: "not-a-sha",
        afterSha: "b".repeat(40),
        created: false,
        deleted: false,
        forced: false,
      }),
    ).toThrow();
  });
});
