import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import {
  createProcessGithubInstallationReceived,
  createProcessGithubInstallationRepositoriesReceived,
} from "./webhook";

describe("GitHub installation webhook Inngest adapters", () => {
  it("decodes a complete valid installation envelope in its owned step", async () => {
    const output = await new InngestTestEngine({
      function: createProcessGithubInstallationReceived(
        new Inngest({ id: "test" }),
      ),
      events: [
        {
          name: "github/installation.received",
          data: {
            deliveryId: "delivery-1",
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
    }).executeStep("decode-installation-event");

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual(
      expect.objectContaining({
        deliveryId: "delivery-1",
        action: "created",
        installation: expect.objectContaining({ id: 17 }),
      }),
    );
  });

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
    "classifies malformed %s ingress inside an owned step",
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
      expect(output.ctx.step.run).toHaveBeenCalledTimes(1);
    },
  );

  it("rejects a valid payload carried by the wrong event envelope", async () => {
    const output = await new InngestTestEngine({
      function: createProcessGithubInstallationReceived(
        new Inngest({ id: "test" }),
      ),
      events: [
        {
          name: "github/installation.received",
          data: {
            deliveryId: "delivery-1",
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
      transformCtx: (ctx) => ({
        ...mockCtx(ctx),
        event: {
          ...ctx.event,
          name: "github/installation-repositories.received",
        },
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
    expect(output.ctx.step.run).toHaveBeenCalledTimes(1);
  });
});
