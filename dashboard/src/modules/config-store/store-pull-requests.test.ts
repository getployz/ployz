import { describe, expect, it } from "vitest";
import type { PrPlan, PullRequestHint, RowId } from "@ployz/sdk";
import { hintNotes, planSummary } from "./store-pull-requests";

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

describe("hintNotes", () => {
  const hint = (name: string): PullRequestHint =>
    ({ conditional_sync: "s", pull_request: 5, row: `w:${name}` as RowId, node: "web", name, value: "x", landed: "hint" });
  it("puts a hint beside its change by its path, the rest after", () => {
    const notes = hintNotes([hint("startCommand"), hint("env.KEY")], new Set(["web.startCommand"]));
    expect(notes.at("web.startCommand").map((note) => note.name)).toEqual(["startCommand"]);
    expect(notes.rest.map((note) => note.name)).toEqual(["env.KEY"]);
  });
});
