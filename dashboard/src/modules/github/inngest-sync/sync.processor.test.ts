import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { createSyncGithubRepositories } from "./sync";

describe("GitHub repository sync Inngest adapter", () => {
  it("fails malformed ingress for good before any step", async () => {
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
    expect(output.ctx.step.run).not.toHaveBeenCalled();
  });
});
