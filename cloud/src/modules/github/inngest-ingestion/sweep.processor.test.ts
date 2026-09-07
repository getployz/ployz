import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import * as repository from "#/modules/github/github-ingestion.repository";
import { createSweepGithubIngestionOutboxes } from "./sweep";

vi.spyOn(
  repository,
  "listPendingGithubEnvironmentTriggers",
).mockImplementation(() => Effect.succeed([]));
vi.spyOn(
  repository,
  "listPendingGithubCheckSuiteTransitions",
).mockImplementation(() => Effect.succeed([]));

describe("GitHub ingestion sweep Inngest adapter", () => {
  it("executes the cron through both durable outbox listing steps", async () => {
    const fn = createSweepGithubIngestionOutboxes(
      new Inngest({ id: "test" }),
    );
    expect(fn.opts.triggers).toEqual([{ cron: "*/5 * * * *" }]);

    const output = await new InngestTestEngine({ function: fn }).execute();

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({
      environmentTriggers: 0,
      checkSuiteTransitions: 0,
    });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "sweep-list-environment-outbox",
      expect.any(Function),
    );
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "sweep-list-check-suite-outbox",
      expect.any(Function),
    );
  });
});
