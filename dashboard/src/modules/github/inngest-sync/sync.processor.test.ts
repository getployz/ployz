import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { githubRepositoriesSyncRequestedEventType } from "#/modules/inngest/events";
import { createSyncGithubRepositories } from "./sync";

describe("GitHub repository sync Inngest adapter", () => {
  it("preserves the trigger, retries, singleton, and keyed concurrency", () => {
    const fn = createSyncGithubRepositories(new Inngest({ id: "test" }));

    expect(fn.opts).toEqual(
      expect.objectContaining({
        id: "sync-github-repositories",
        retries: 3,
        triggers: [{ event: githubRepositoriesSyncRequestedEventType }],
        singleton: {
          key: "event.data.installationId",
          mode: "skip",
        },
        concurrency: [{ key: "event.data.installationId", limit: 1 }],
      }),
    );
  });

  it("classifies malformed ingress inside the durable adapter", async () => {
    const output = await new InngestTestEngine({
      function: createSyncGithubRepositories(new Inngest({ id: "test" })),
      events: [
        { name: "github/repositories-sync.requested", data: {} },
      ],
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
