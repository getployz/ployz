import type { DeploymentSummary, RowId, SyncRow } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { closesIn, goesLive, syncButtonState, syncLine, syncPicks, syncSections } from "./store-sync";

// A row by its RowId, node and name in the node; no name brings the node whole.
const row = (id: string, node: string, name: string | null, extra: Partial<SyncRow> = {}): SyncRow => ({
  row: id as RowId, node, kind: "service", name, change: "changed", from: null, into: null, ticked: true, requires: null, secret: null, held_by: null, ...extra,
});
const secret = { held: false };

const image = row("a:source", "api", "source", {
  from: { type: "image", image: "shop/api:1.9", credentials: false }, into: { type: "image", image: "shop/api:1.8", credentials: false },
});
const logLevel = row("a:variables.LOG_LEVEL", "api", "env.LOG_LEVEL", { from: "debug", into: "warn", change: "conflict" });
const appEnv = row("a:variables.APP_ENV", "api", "env.APP_ENV", { from: "staging", into: "production", ticked: false });
const webhook = row("a:variables.STRIPE_WEBHOOK_SECRET", "api", "env.STRIPE_WEBHOOK_SECRET",
  { from: { secret: true }, change: "new", secret });
const cache = row("c:node", "cache", null, { change: "new" });
const cacheMode = row("c:variables.MODE", "cache", "env.MODE", { from: "lru", change: "new", requires: cache.row });
const dataName = row("d:name", "volumes.data", "name", { from: "pg", into: "data" });

describe("the Sync dialog over the Store", () => {
  it("words each change by name, with at most one badge, Secret first, and hides a secret's value", () => {
    const lines = [image, logLevel, webhook, cache, cacheMode, dataName].map((one) => syncLine(one, "production"));
    expect(lines).toEqual([
      { name: "Source", variable: false, badge: null, before: "shop/api:1.8", after: "shop/api:1.9" },
      { name: "LOG_LEVEL", variable: true, badge: "Changed in production", before: "warn", after: "debug" },
      { name: "STRIPE_WEBHOOK_SECRET", variable: true, badge: "Secret", before: "", after: "" },
      { name: "Service", variable: false, badge: "New", before: "", after: "" },
      { name: "MODE", variable: true, badge: "New", before: "", after: "lru" },
      { name: "Name", variable: false, badge: null, before: "data", after: "pg" },
    ]);
  });

  it("groups the rows by node in the Store's order", () => {
    expect(syncSections([image, cache, logLevel, cacheMode]).map(({ node, rows }) => [node, rows.map((one) => one.row)])).toEqual([
      ["api", ["a:source", "a:variables.LOG_LEVEL"]],
      ["cache", ["c:node", "c:variables.MODE"]],
    ]);
  });

  it("picks the defaults, then what the user flipped; a new node left out leaves its settings out", () => {
    const rows = [image, appEnv, cache, cacheMode];
    expect(syncPicks(rows, new Set()).map((one) => one.row)).toEqual(["a:source", "c:node", "c:variables.MODE"]);
    expect(syncPicks(rows, new Set([image.row, appEnv.row, cache.row])).map((one) => one.row))
      .toEqual(["a:variables.APP_ENV"]);
    // A new node's setting can be left out on its own.
    expect(syncPicks(rows, new Set([cacheMode.row])).map((one) => one.row)).toEqual(["a:source", "c:node"]);
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
    const merge = { number: 142, into: "production", changes: 3, standing: null };
    const off = asTestDouble<DeploymentSummary>()({ status: "applied", in_flight: false });
    expect([
      syncButtonState(pr, null, merge),
      syncButtonState(pr, null, { ...merge, standing: "cs-1" }),
      syncButtonState(pr, off, { ...merge, standing: "cs-1" }),
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
