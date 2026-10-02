import { describe, expect, it } from "vitest";
import type { PrPlan, PullRequestHint, PullRequestView } from "@ployz/sdk";
import { destinationNews, hintNotes, planSummary } from "./store-pull-requests";

const plan: PrPlan = {
  repository: "acme/web", repository_id: 11, installation_id: 7, enabled: true, start_from: "production",
  copy: ["db"], setup: [{ service: "web", command: "php artisan migrate" }], remove_on_close: true, include_bots: false, open: [],
};

describe("planSummary", () => {
  it("words a plan", () => {
    expect(planSummary({ ...plan, enabled: false })).toBe("Off");
    expect(planSummary({ ...plan, start_from: null })).toBe("On · pick an environment to start from");
    expect(planSummary(plan)).toBe("On · from production · db copied · then php artisan migrate");
  });
});

const summary = { id: "e", project: "shop", name: "pr-5", revision: 1 };
const view = (destinations: PullRequestView["environments"][number]["destinations"]): PullRequestView => ({
  pull_request: {
    repository_id: 11, number: 5, title: "Search", author: "ada", bot: false, head_branch: "search", head: "h", target_branch: "main",
    commits: 1, open: true, merge_commit: null, merge_reached: null, updated: "2026-09-29T10:00:00Z",
  },
  environments: [{ environment: summary, deployment: null, destinations }],
  passing: false,
  reason: "",
});

describe("destinationNews", () => {
  it("reads each Destination as to save, saved or stale", () => {
    expect(destinationNews(view([
      { name: "production", changes: 2, conditional_sync: null },
      { name: "staging", changes: 0, conditional_sync: { id: "s", standing: true, changes: 3 } },
      { name: "eu", changes: 1, conditional_sync: { id: "t", standing: false, changes: 2 } },
      { name: "quiet", changes: 0, conditional_sync: null },
    ]), "pr-5")).toEqual([
      { kind: "save", into: "production", changes: 2 },
      { kind: "saved", into: "staging", changes: 3, save: "s" },
      { kind: "stale", into: "eu", changes: 2, save: "t" },
    ]);
  });

  it("finds nothing for another Environment", () => {
    expect(destinationNews(view([{ name: "production", changes: 2, conditional_sync: null }]), "pr-6")).toEqual([]);
  });
});

describe("hintNotes", () => {
  const hint = (row: string): PullRequestHint => ({ save: "s", pull_request: 5, row, value: "x", landed: "hint" });
  it("puts a hint beside its change, the rest after", () => {
    const notes = hintNotes([hint("web.startCommand"), hint("web.env.KEY")], new Set(["web.startCommand"]));
    expect(notes.at("web.startCommand").map((note) => note.row)).toEqual(["web.startCommand"]);
    expect(notes.rest.map((note) => note.row)).toEqual(["web.env.KEY"]);
  });
});
