import type { ConfigCommand, ConfigWritten, EnvironmentListing, JsonValue } from "@ployz/sdk";
import { expect, it } from "vitest";
import { storeEnvironmentTree, teardownStep } from "./store-workspace";
import { StoreRefused } from "./store.contract";

const refused = (code: string, details: JsonValue) => new StoreRefused({ code, message: code, details });

/** A Store that answers each command in turn, recording what it was sent. */
function store(...answers: Array<ConfigWritten | StoreRefused>) {
  const sent: ConfigCommand[] = [];
  const commit = (command: ConfigCommand) => {
    sent.push(command);
    const answer = answers.shift() ?? admitted;
    return answer instanceof StoreRefused ? Promise.reject(answer) : Promise.resolve(answer);
  };
  return { sent, commit };
}

const admitted = { written: "deployment" } as ConfigWritten;
const removed = { written: "environment_removed", teardown: "removed" } as ConfigWritten;
const onServers = (teardown: "waiting" | "needs_removal") =>
  ({ written: "project_removed", teardown, environment: "staging", deployment: "d1" }) as ConfigWritten;

it("deletes at once what never ran on the Servers", async () => {
  const { sent, commit } = store(removed);
  expect(await teardownStep(commit, { project: "shop", environment: "staging" }, {})).toEqual({ done: true });
  expect(sent).toEqual([{ command: "remove_environment", environment: { project: "shop", environment: "staging" } }]);
});

it("takes an Environment still on the Servers off them first, accepting only its own Volumes' loss", async () => {
  const { sent, commit } = store(onServers("needs_removal"));
  const step = await teardownStep(commit, { project: "shop", environment: null }, { staging: ["pg"], production: ["files"] });
  expect(sent[0]).toEqual({ command: "remove_project", project: "shop" });
  expect(sent[1]).toMatchObject({ command: "admit", admit: "remove", environment: { project: "shop", environment: "staging" }, accept_volume_loss: ["pg"] });
  expect(step).toEqual({ done: false, environment: "staging", deployment: (sent[1] as { id: string }).id, admitted: true });
});

it("waits on a Deployment that hasn't ended rather than admitting another", async () => {
  const { sent, commit } = store(onServers("waiting"));
  expect(await teardownStep(commit, { project: "shop", environment: "staging" }, {}))
    .toEqual({ done: false, environment: "staging", deployment: "d1", admitted: false });
  expect(sent).toHaveLength(1);
});

it("closes one Environment it takes off the Servers, and a Branch the Store already deleted is done", async () => {
  const { sent, commit } = store({ ...onServers("needs_removal"), written: "environment_removed" } as ConfigWritten, admitted, refused("not_found", null));
  const target = { project: "shop", environment: "staging" };
  await teardownStep(commit, target, {});
  expect(sent[1]).toMatchObject({ command: "admit", admit: "remove", close: true });
  expect(await teardownStep(commit, target, {})).toEqual({ done: true });
});

it("passes any refusal on", async () => {
  const { commit } = store(refused("conflict", { next: "ployz env default staging" }));
  await expect(teardownStep(commit, { project: "shop", environment: "production" }, {})).rejects.toThrow("conflict");
});

it("lists each Branch right under its Parent", () => {
  const listing = (name: string, parent: string | null = null): EnvironmentListing => ({ id: name, name, default: false, parent, removal: null, branch_setup: [] });
  const tree = storeEnvironmentTree([listing("fix", "staging"), listing("production"), listing("staging"), listing("orphan", "gone")]);
  expect(tree.map(({ environment, depth }) => `${depth}:${environment.name}`)).toEqual(["0:production", "0:staging", "1:fix", "0:orphan"]);
});
