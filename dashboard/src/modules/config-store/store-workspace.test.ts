import type { ConfigCommand, EnvironmentListing, JsonValue } from "@ployz/sdk";
import { expect, it } from "vitest";
import { storeEnvironmentTree, teardownStep } from "./store-workspace";
import { StoreRefused } from "./store-write";

const refused = (code: string, details: JsonValue) => new StoreRefused({ code, message: code, details });

/** A Store that answers each command in turn, recording what it was sent. */
function store(...answers: Array<StoreRefused | null>) {
  const sent: ConfigCommand[] = [];
  const commit = (command: ConfigCommand) => {
    sent.push(command);
    const answer = answers.shift();
    return answer ? Promise.reject(answer) : Promise.resolve({ written: "environment_removed" } as never);
  };
  return { sent, commit };
}

it("deletes at once what never ran on the Servers", async () => {
  const { sent, commit } = store(null);
  expect(await teardownStep(commit, { project: "shop", environment: "staging" }, {})).toEqual({ done: true });
  expect(sent).toEqual([{ command: "remove_environment", environment: { project: "shop", environment: "staging" } }]);
});

it("takes an Environment still on the Servers off them first, accepting only its own Volumes' loss", async () => {
  const { sent, commit } = store(refused("conflict", { deployed: true, deployment: "d1", environment: "staging" }), null);
  const step = await teardownStep(commit, { project: "shop", environment: null }, { staging: ["pg"], production: ["files"] });
  expect(sent[0]).toEqual({ command: "remove_project", project: "shop" });
  expect(sent[1]).toMatchObject({ command: "admit", environment: { project: "shop", environment: "staging" }, remove: true, accept_volume_loss: ["pg"] });
  expect(step).toEqual({ done: false, environment: "staging", deployment: (sent[1] as { id: string }).id });
});

it("waits on a Deployment that hasn't ended rather than admitting another", async () => {
  const { sent, commit } = store(refused("conflict", { deployment: "d1", environment: "staging" }));
  expect(await teardownStep(commit, { project: "shop", environment: "staging" }, {}))
    .toEqual({ done: false, environment: "staging", deployment: "d1" });
  expect(sent).toHaveLength(1);
});

it("passes any other refusal on", async () => {
  const { commit } = store(refused("conflict", { next: "ployz env default staging" }));
  await expect(teardownStep(commit, { project: "shop", environment: "production" }, {})).rejects.toThrow("conflict");
});

it("lists each Branch right under its Parent", () => {
  const listing = (name: string, parent: string | null = null): EnvironmentListing => ({ id: name, name, default: false, parent, removal: null });
  const tree = storeEnvironmentTree([listing("fix", "staging"), listing("production"), listing("staging"), listing("orphan", "gone")]);
  expect(tree.map(({ environment, depth }) => `${depth}:${environment.name}`)).toEqual(["0:production", "0:staging", "1:fix", "0:orphan"]);
});
