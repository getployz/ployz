import { fakeText } from "@tanstack/ai/testing";
import { describe, expect, it } from "vitest";
import { simulated } from "./simulator";

const persona = { goal: "Deploy the api.", opening: "deploy the api", facts: [] };

describe("simulated", () => {
  it("replies with the model's text and stops on the stop word", async () => {
    const adapter = fakeText();
    adapter.setResponses([{ text: "yes, the api" }, { text: "###STOP###" }]);
    const member = simulated(adapter, persona);

    expect(await member.open()).toBe("deploy the api");
    expect(await member.reply("Which service?")).toBe("yes, the api");
    expect(await member.reply("Deployed.")).toBeNull();
  });
});
