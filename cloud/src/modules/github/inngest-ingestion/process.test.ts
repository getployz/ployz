import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import {
  GITHUB_CHECK_SUITE_RECEIVED_CONCURRENCY,
  GITHUB_PUSH_RECEIVED_CONCURRENCY,
} from "#/modules/github/inngest-ingestion/process";
import {
  createProcessGithubCheckSuiteReceived,
  createProcessGithubPushReceived,
} from "#/modules/github/inngest-ingestion/process";

describe("GitHub ingestion workflow contracts", () => {
  it("serializes branch and check-suite authorities with stable concurrency keys", () => {
    expect(GITHUB_PUSH_RECEIVED_CONCURRENCY).toEqual([
      { key: "event.data.branchKey", limit: 1 },
    ]);
    expect(GITHUB_CHECK_SUITE_RECEIVED_CONCURRENCY).toEqual([
      { key: "event.data.checkSuiteKey", limit: 1 },
    ]);
  });

  it.each([
    ["push", "github/push.received", createProcessGithubPushReceived],
    [
      "check suite",
      "github/check-suite.received",
      createProcessGithubCheckSuiteReceived,
    ],
  ] as const)(
    "rejects malformed %s event data before any activity",
    async (_, eventName, createFunction) => {
      const output = await new InngestTestEngine({
        function: createFunction(new Inngest({ id: "test" })),
        events: [{ name: eventName, data: {} }],
      }).execute();

      expect(output.error).toEqual(
        expect.objectContaining({
          message: expect.stringContaining("event envelope is invalid"),
          stack: expect.stringContaining("NonRetriableError"),
        }),
      );
      expect(output.ctx.step.run).toHaveBeenCalledTimes(1);
    },
  );

  it("decodes a complete valid push envelope in its owned step", async () => {
    const output = await new InngestTestEngine({
      function: createProcessGithubPushReceived(
        new Inngest({ id: "test" }),
      ),
      events: [
        {
          name: "github/push.received",
          data: {
            deliveryId: "delivery-1",
            kind: "push",
            installationId: 17,
            repositoryId: 42,
            ref: "refs/heads/main",
            branch: "main",
            beforeSha: "a".repeat(40),
            afterSha: "b".repeat(40),
            created: false,
            deleted: false,
            forced: false,
            branchKey: "17:42:refs/heads/main",
          },
        },
      ],
    }).executeStep("decode-push-event");

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual(
      expect.objectContaining({
        deliveryId: "delivery-1",
        branchKey: "17:42:refs/heads/main",
      }),
    );
  });

  it("rejects valid push data carried by the wrong event envelope", async () => {
    const output = await new InngestTestEngine({
      function: createProcessGithubPushReceived(
        new Inngest({ id: "test" }),
      ),
      events: [
        {
          name: "github/push.received",
          data: {
            deliveryId: "delivery-1",
            kind: "push",
            installationId: 17,
            repositoryId: 42,
            ref: "refs/heads/main",
            branch: "main",
            beforeSha: "a".repeat(40),
            afterSha: "b".repeat(40),
            created: false,
            deleted: false,
            forced: false,
            branchKey: "17:42:refs/heads/main",
          },
        },
      ],
      transformCtx: (ctx) => ({
        ...mockCtx(ctx),
        event: { ...ctx.event, name: "github/check-suite.received" },
      }),
    }).execute();

    expect(output.error).toEqual(
      expect.objectContaining({
        message: "Inngest event envelope is invalid.",
        stack: expect.stringContaining("NonRetriableError"),
      }),
    );
    expect(output.ctx.step.run).toHaveBeenCalledTimes(1);
  });
});
