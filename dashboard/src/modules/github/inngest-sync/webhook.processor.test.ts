import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import {
  createProcessGithubInstallationReceived,
  createProcessGithubInstallationRepositoriesReceived,
} from "./webhook";

describe("GitHub installation webhook Inngest adapters", () => {
  it.each([
    [
      "installation",
      "github/installation.received",
      createProcessGithubInstallationReceived,
    ],
    [
      "installation repositories",
      "github/installation-repositories.received",
      createProcessGithubInstallationRepositoriesReceived,
    ],
  ] as const)(
    "fails malformed %s ingress for good before any step",
    async (_, eventName, createFunction) => {
      const output = await new InngestTestEngine({
        function: createFunction(new Inngest({ id: "test" })),
        events: [{ name: eventName, data: {} }],
      }).execute();

      expect(output.error).toEqual(
        expect.objectContaining({
          message: "Inngest event envelope is invalid.",
          stack: expect.stringContaining("NonRetriableError"),
        }),
      );
      expect(output.ctx.step.run).not.toHaveBeenCalled();
    },
  );

  it("rejects an installation payload whose decoded event data lacks a delivery id", async () => {
    const output = await new InngestTestEngine({
      function: createProcessGithubInstallationReceived(
        new Inngest({ id: "test" }),
      ),
      events: [
        {
          name: "github/installation.received",
          data: {
            action: "created",
            installation: {
              id: 17,
              account: {
                login: "octocat",
                type: "User",
                avatar_url: "https://example.test/avatar.png",
              },
            },
            sender: { id: 23, login: "octocat" },
          },
        },
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
