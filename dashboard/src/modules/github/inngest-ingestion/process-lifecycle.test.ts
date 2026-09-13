import { describe, expect, it, vi } from "vitest";
import { Effect } from "effect";
import {
  failGithubIngestionDeliveryOnRetryExhausted,
  type GithubIngestionEffectRunner,
} from "#/modules/github/inngest-ingestion/process";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { InngestClient } from "#/modules/inngest/client";

describe("GitHub ingestion terminal lifecycle", () => {
  it("does not run a terminal activity for a partial failure envelope", async () => {
    const invoked = vi.fn();
    const runEffect: GithubIngestionEffectRunner = (effect) => {
      invoked();
      return Effect.runPromise(effect.pipe(
        Effect.provide(AppConfig.layer),
        // SAFETY: This runner is asserted unused when the run id is absent.
        Effect.provideService(Database, undefined as never),
        // SAFETY: This runner is asserted unused when the run id is absent.
        Effect.provideService(InngestClient, undefined as never),
        // SAFETY: This runner is asserted unused when the run id is absent.
        Effect.provideService(GithubApi, undefined as never),
      ));
    };

    await failGithubIngestionDeliveryOnRetryExhausted(
      {
        name: "inngest/function.failed",
        data: { run_id: "run-1" },
      },
      runEffect,
    );

    expect(invoked).not.toHaveBeenCalled();
  });

  it("runs the terminal activity for a complete failure envelope", async () => {
    const invoked = vi.fn();
    const runEffect: GithubIngestionEffectRunner = () => {
      invoked();
      return Promise.resolve(undefined as never);
    };

    await failGithubIngestionDeliveryOnRetryExhausted(
      {
        name: "inngest/function.failed",
        data: {
          function_id: "process-github-push-received",
          run_id: "run-1",
          error: { name: "Error", message: "retry budget exhausted" },
          event: {
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
        },
      },
      runEffect,
    );

    expect(invoked).toHaveBeenCalledTimes(1);
  });
});
