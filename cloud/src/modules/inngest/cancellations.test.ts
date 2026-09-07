import { describe, expect, it, vi } from "vitest";
import { Effect } from "effect";
import type { GithubIngestionEffectRunner } from "#/modules/github/inngest-ingestion/process";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { InngestClient, type PloyzStepTools } from "#/modules/inngest/client";
import { executeCancelGithubRowBackedWorkflow } from "./cancellations";

describe("row-backed Inngest cancellation handling", () => {
  it("ignores functions that do not own GitHub delivery rows", async () => {
    const invoked = vi.fn();
    const runEffect: GithubIngestionEffectRunner = (effect) => {
      invoked();
      return Effect.runPromise(effect.pipe(
        Effect.provide(AppConfig.layer),
        // SAFETY: This runner is asserted unused by the malformed-event test.
        Effect.provideService(Database, undefined as never),
        // SAFETY: This runner is asserted unused by the malformed-event test.
        Effect.provideService(InngestClient, undefined as never),
        // SAFETY: This runner is asserted unused by the malformed-event test.
        Effect.provideService(GithubApi, undefined as never),
      ));
    };

    const run: PloyzStepTools["run"] = async (_id, operation, ...input) => {
      const value = await operation(...input);
      return value === undefined ? null : JSON.parse(JSON.stringify(value));
    };
    const result = await executeCancelGithubRowBackedWorkflow(
      {
        functionId: "unrelated-function",
        runId: "unrelated-run",
        step: { run },
      },
      runEffect,
    );

    expect(result).toEqual({
      functionId: "unrelated-function",
      runId: "unrelated-run",
      skipped: true,
    });
    expect(invoked).not.toHaveBeenCalled();
  });
});
