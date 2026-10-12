import type { ConfigCommand, ConfigWritten, EnvironmentListing, JsonValue } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { removeTarget, storeEnvironmentTree } from "./store-workspace";
import { StoreRefused } from "./store.contract";

const refused = (code: string, details: JsonValue) => new StoreRefused({ code, message: code, details });

const removed = asTestDouble<ConfigWritten>()({ written: "removal", removal: "removed", residuals: [] });

/** A Store that answers each command in turn, recording what it was sent. */
function store(...answers: Array<ConfigWritten | StoreRefused>) {
  const sent: ConfigCommand[] = [];
  const commit = (command: ConfigCommand) => {
    sent.push(command);
    const answer = answers.shift() ?? removed;
    return answer instanceof StoreRefused ? Promise.reject(answer) : Promise.resolve(answer);
  };
  return { sent, commit };
}

const shop = { project: "shop", environment: null };
const none = { accept: {}, version: null, everyCopy: {} };

it("deletes in one remove, which the Store finishes on its own", async () => {
  const { sent, commit } = store(removed);
  expect(await removeTarget(commit, { project: "shop", environment: "staging" }, none)).toEqual({ sent: removed });
  expect(sent).toEqual([{ command: "remove", project: "shop", environment: "staging", version: null, accept_volume_loss: {}, every_copy: {} }]);
});

it("sends the version the Store reviewed back at once when it loses exactly what the user accepted", async () => {
  const { sent, commit } = store(refused("confirmation_required", { version: "p:1", accept: { staging: ["pg"] } }), removed);
  const answer = await removeTarget(commit, shop, { ...none, accept: { staging: ["pg"], production: [] } });
  expect(answer).toEqual({ sent: removed });
  expect(sent[1]).toMatchObject({ command: "remove", project: "shop", version: "p:1", accept_volume_loss: { staging: ["pg"] } });
});

it("asks the user again when the Store loses Volumes they didn't see", async () => {
  const { sent, commit } = store(refused("confirmation_required", { version: "p:2", accept: { staging: ["pg", "cache"] } }));
  const answer = await removeTarget(commit, shop, { ...none, accept: { staging: ["pg"] } });
  expect(answer).toEqual({ ask: { accept: { staging: ["pg", "cache"] }, version: "p:2", everyCopy: {} }, waitingOn: [] });
  expect(sent).toHaveLength(1);
});

it("asks before giving up every copy a Server that didn't answer may hold", async () => {
  const waiting = { server: "m1", why: "It didn't answer", fix: { server_remove: { server: "m1" } } };
  const offer = { tool: "remove", input: { project: "shop", accept_volume_loss: { staging: ["pg"] }, every_copy: { staging: ["pg"] }, version: "p:3" } };
  const { commit } = store(refused("unavailable", { waiting_on: [waiting], every_copy: offer }));
  expect(await removeTarget(commit, shop, none))
    .toEqual({ ask: { accept: { staging: ["pg"] }, version: "p:3", everyCopy: { staging: ["pg"] } }, waitingOn: [{ server: "m1", why: "It didn't answer" }] });
});

it("passes any other refusal on", async () => {
  const { commit } = store(refused("unsupported", { branches: ["fix"] }));
  await expect(removeTarget(commit, { project: "shop", environment: "production" }, none)).rejects.toThrow("unsupported");
});

it("lists each Branch right under its Parent", () => {
  const listing = (name: string, parent: string | null = null): EnvironmentListing => ({ id: name, name, default: false, parent, removal: null, merged: false, branch_setup: [] });
  const tree = storeEnvironmentTree([listing("fix", "staging"), listing("production"), listing("staging"), listing("orphan", "gone")]);
  expect(tree.map(({ environment, depth }) => `${depth}:${environment.name}`)).toEqual(["0:production", "0:staging", "1:fix", "0:orphan"]);
});
