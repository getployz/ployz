import { useServiceFreeEffectRunner } from "#/test/service-free-effect-runner";
import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import * as repository from "#/modules/github/github.repository";
import { createScheduleGithubRepositorySync } from "./scheduled";

useServiceFreeEffectRunner();

vi.spyOn(repository, "listAllGithubInstallationIds").mockImplementation(() =>
  Effect.succeed([17]),
);

describe("scheduled GitHub repository sync Inngest adapter", () => {
  it("executes the cron through its durable list and send steps", async () => {
    const fn = createScheduleGithubRepositorySync(
      new Inngest({ id: "test" }),
    );
    expect(fn.opts.triggers).toEqual([{ cron: "TZ=UTC 0 2 * * *" }]);

    const output = await new InngestTestEngine({
      function: fn,
      steps: [
        {
          id: "request-repository-syncs",
          handler: () => ({ ids: ["event-1"] }),
        },
      ],
    }).execute();

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({ installationCount: 1 });
    expect(output.ctx.step.run).toHaveBeenCalledWith(
      "list-installation-ids",
      expect.any(Function),
    );
    expect(output.ctx.step.sendEvent).toHaveBeenCalledWith(
      "request-repository-syncs",
      [
        {
          name: "github/repositories-sync.requested",
          data: { installationId: 17, reason: "scheduled" },
        },
      ],
    );
  });
});
