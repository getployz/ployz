import type { DeploymentSummary, SyncRow } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { closesIn, goesLive, syncButtonState, syncLine, syncPicks, syncSections, undoPaths } from "./store-sync";

// A row named by its receiver path; one naming only its node brings the node whole.
const row = (key: string, node: string, path: string, extra: Partial<SyncRow> = {}): SyncRow => ({
  key, node, label: path, path, whole: path === node, from: null, into: null, ticked: true, changed: false, new: false,
  secret: false, value_set: false, ...extra,
});

const image = row("a:source.image", "api", "api.image", { from: "shop/api:1.9", into: "shop/api:1.8" });
const logLevel = row("a:variables.LOG_LEVEL", "api", "api.env.LOG_LEVEL", { from: "debug", into: "warn", changed: true });
const appEnv = row("a:variables.APP_ENV", "api", "api.env.APP_ENV", { from: "staging", into: "production", ticked: false });
const webhook = row("a:variables.STRIPE_WEBHOOK_SECRET", "api", "api.env.STRIPE_WEBHOOK_SECRET",
  { from: { secret: true }, new: true, secret: true, changed: true });
const cache = row("c:node", "cache", "cache", { new: true });
const cacheMode = row("c:variables.MODE", "cache", "cache.env.MODE", { from: "lru", new: true });
const dataName = row("d:name", "volumes.data", "volumes.data.name", { from: "pg", into: "data" });

describe("the Sync dialog over the Store", () => {
  it("words each change by name, with at most one badge, Secret first, and hides a secret's value", () => {
    const lines = [image, logLevel, webhook, cache, cacheMode, dataName].map((one) => syncLine(one, "production"));
    expect(lines).toEqual([
      { name: "Container image", variable: false, badge: null, before: "shop/api:1.8", after: "shop/api:1.9" },
      { name: "LOG_LEVEL", variable: true, badge: "Changed in production", before: "warn", after: "debug" },
      { name: "STRIPE_WEBHOOK_SECRET", variable: true, badge: "Secret", before: "", after: "" },
      { name: "Service", variable: false, badge: "New", before: "", after: "" },
      { name: "MODE", variable: true, badge: "New", before: "", after: "lru" },
      { name: "Name", variable: false, badge: null, before: "data", after: "pg" },
    ]);
  });

  it("groups the rows by node in the Store's order", () => {
    expect(syncSections([image, cache, logLevel, cacheMode]).map(({ node, rows }) => [node, rows.map(({ key }) => key)])).toEqual([
      ["api", ["a:source.image", "a:variables.LOG_LEVEL"]],
      ["cache", ["c:node", "c:variables.MODE"]],
    ]);
  });

  it("picks the defaults, then what the user flipped; a new node left out leaves its settings out", () => {
    const rows = [image, appEnv, cache, cacheMode];
    expect(syncPicks(rows, new Set()).map(({ key }) => key)).toEqual(["a:source.image", "c:node", "c:variables.MODE"]);
    expect(syncPicks(rows, new Set(["a:source.image", "a:variables.APP_ENV", "c:node"])).map(({ key }) => key))
      .toEqual(["a:variables.APP_ENV"]);
    // A new node's setting can be left out on its own.
    expect(syncPicks(rows, new Set(["c:variables.MODE"])).map(({ key }) => key)).toEqual(["a:source.image", "c:node"]);
  });

  it("undoes a Sync by discarding each synced setting by its receiver path, and a new node whole", () => {
    expect(undoPaths([image, logLevel, cache, cacheMode, dataName]))
      .toEqual(["api.image", "api.env.LOG_LEVEL", "cache", "volumes.data.name"]);
  });
});

describe("the Sync button", () => {
  const branch = { parent: "production", to_parent: 4, pull_request: null };
  const pr = { ...branch, pull_request: { repository_id: 1, number: 142 } };
  const removal = (status: DeploymentSummary["status"], in_flight: boolean) => asTestDouble<DeploymentSummary>()({ status, in_flight });

  it("says the first that applies: a shutdown under way or failed, Off, then what syncs into the Parent", () => {
    expect([
      syncButtonState(pr, removal("running", true)),
      syncButtonState(pr, removal("failed", false)),
      syncButtonState(pr, removal("applied", false)),
      syncButtonState(branch, removal("running", true)),
      syncButtonState(branch, null),
      syncButtonState({ ...branch, to_parent: 0 }, null),
    ].map(({ label, count }) => ({ label, count }))).toEqual([
      { label: "Shutting down", count: null },
      { label: "Shutdown failed", count: null },
      { label: "Off", count: null },
      { label: "Closing", count: null },
      { label: "Sync to production", count: 4 },
      { label: "In sync with production", count: null },
    ]);
    // Where its Sync goes, and what that carries, whatever it says.
    expect(syncButtonState(pr, removal("applied", false))).toMatchObject({ into: "production", changes: 4 });
  });

  it("reads a PR Environment's Destination: Goes live once its Conditional Sync stands, after Off", () => {
    const pr = { parent: "staging", to_parent: 1, pull_request: { repository_id: 1, number: 142 } };
    const merge = { number: 142, into: "production", changes: 3, standing: false };
    const off = asTestDouble<DeploymentSummary>()({ status: "applied", in_flight: false });
    expect([
      syncButtonState(pr, null, merge),
      syncButtonState(pr, null, { ...merge, standing: true }),
      syncButtonState(pr, off, { ...merge, standing: true }),
      goesLive(1, "production", 142),
    ]).toEqual([
      { label: "Sync to production", into: "production", changes: 3, count: 3 },
      { label: "Goes live with #142", into: "production", changes: 3, count: null },
      { label: "Off", into: "production", changes: 3, count: null },
      "1 change goes live in production when #142 merges",
    ]);
  });

  it("says in whole days when an idle Branch closes", () => {
    const day = 24 * 60 * 60;
    expect([closesIn(5 * day, 0), closesIn(4.2 * day, 0), closesIn(60, 0), closesIn(0, 60)])
      .toEqual(["Closes in 5 days", "Closes in 5 days", "Closes in 1 day", "Closes in 1 day"]);
  });
});
