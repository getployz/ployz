import type { ConfigCommand, ConfigWritten, DeploymentStatus, EnvironmentListing, EnvironmentRemoval, JsonValue } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { removalDeployment, removeTarget, removing, storeEnvironmentNotes, storeEnvironmentTree } from "./store-workspace";
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
  const listing = (name: string, parent: string | null = null): EnvironmentListing => ({ id: name, name, default: false, parent, removal: null, merged: false, branch_setup: [], protected: false, protections: 0 });
  const tree = storeEnvironmentTree([listing("fix", "staging"), listing("production"), listing("staging"), listing("orphan", "gone")]);
  expect(tree.map(({ environment, depth }) => `${depth}:${environment.name}`)).toEqual(["0:production", "0:staging", "1:fix", "0:orphan"]);
});

it("notes a close waiting for the sweep apart from a removal that runs or ended", () => {
  const listing = (removal: EnvironmentListing["removal"]): EnvironmentListing =>
    ({ id: "fix", name: "fix", default: false, parent: "production", removal, merged: false, branch_setup: [], protected: false, protections: 0 });
  const deployment = (status: DeploymentStatus, in_flight: boolean) =>
    asTestDouble<EnvironmentRemoval>()({ kind: "deployment", status, in_flight });
  const listings = [
    listing({ kind: "pending", because: "synced", since: 0 }),
    listing(deployment("running", true)),
    listing(deployment("applied", false)),
    listing(deployment("failed", false)),
    listing(null),
  ];
  expect(listings.map(storeEnvironmentNotes)).toEqual([["deletion pending"], ["deleting"], ["off your servers"], ["deletion failed"], []]);
  expect(listings.map(removing)).toEqual([false, true, false, false, false]);
  expect(listings.map((one) => removalDeployment(one)?.status ?? null)).toEqual([null, "running", "applied", "failed", null]);
});
