import { describe, expect, it } from "vitest";
import type { FollowHint, MergeRow, OnMerge, OnMergeCheck, PrPlan, PullRequestView, RowId } from "@ployz/sdk";
import { CHECK_SUMMARY_LIMIT, hintNotes, onMergeCount, onMergeLists, planSummary, storePrCheckRun } from "./store-pull-requests";

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
  const hint = (name: string): FollowHint =>
    ({ from: "production", row: `w:${name}` as RowId, node: "web", kind: "service", name, value: "x" });
  it("puts a hint beside the change in its row, the rest after", () => {
    const notes = hintNotes([hint("startCommand"), hint("env.KEY")], new Set(["w:startCommand" as RowId]));
    expect(notes.at("w:startCommand" as RowId).map((note) => note.name)).toEqual(["startCommand"]);
    expect(notes.rest.map((note) => note.name)).toEqual(["env.KEY"]);
  });
});

describe("the On merge list", () => {
  const removal = (node: string, extra: Partial<MergeRow>): MergeRow => ({
    row: `${node}:node` as RowId, node, kind: "service", name: null, production: { image: `${node}:1` }, preview: null, lands: null,
    ride: "rides", conflict: false, requires: null, ...extra,
  });
  const merge = (rows: MergeRow[]): OnMerge => ({
    environment: { id: "pr", project: "shop", name: "pr-5", revision: 1 }, pull_request: { repository_id: 1, number: 5 },
    destinations: [{ name: "production", rows }],
  });
  const view = merge([
    removal("db", {}),
    removal("cache", { refused_by: [{ row: "w:variables.CACHE" as RowId, node: "web", kind: "service", name: "env.CACHE" }] }),
    removal("volumes.data", { kind: "volume", ride: "kept", lands: { size: "10GB" }, production: { size: "10GB" } }),
    removal("worker", { ride: "production_keeps" }),
  ]);

  it("lists a Kept Volume but counts only the removals the merge lands", () => {
    expect(onMergeLists(view)[0]?.rows.map((row) => [row.node, row.ride])).toEqual([["db", "rides"], ["cache", "rides"], ["volumes.data", "kept"]]);
    expect(onMergeCount(view)).toBe(1);
  });
});

describe("storePrCheckRun", () => {
  const view = (check: OnMergeCheck | null): PullRequestView => ({
    pull_request: {
      repository_id: 42, number: 146, title: "Add search", author: "ada", author_id: 7, bot: false, head_repository_id: 42, head_branch: "search", head: "a".repeat(40),
      target_branch: "main", commits: 1, open: true, merge_commit: null, target_head: null, updated: "2026-10-11T10:00:00Z",
    },
    environments: [{
      environment: { id: "e1", project: "shop", name: "pr-146", revision: 3 },
      deployment: null, destinations: [],
    }],
    check,
  });
  const lands: OnMergeCheck = { conclusion: "success", title: "1 config change", summary: "- `web · A`: 1 → 2", merged_into: null };
  const missing: OnMergeCheck = {
    conclusion: "neutral", title: "1 config change · TOKEN not set", summary: "- `web · TOKEN`: not set", merged_into: null,
  };

  it("posts what the merge lands, linking the On merge panel", () => {
    expect(storePrCheckRun([{ slug: "acme", view: view(lands) }], "https://ployz.test/")).toEqual({
      headSha: "a".repeat(40),
      name: "Ployz · ready to merge",
      conclusion: "success",
      detailsUrl: "https://ployz.test/cloud/acme/shop/pr-146?panel=on-merge",
      title: "1 config change",
      summary: "- `web · A`: 1 → 2\n\n[View in Ployz](https://ployz.test/cloud/acme/shop/pr-146?panel=on-merge)",
    });
  });

  it("reads neutral when any Organization's word is neutral, and joins their words", () => {
    const run = storePrCheckRun([{ slug: "acme", view: view(lands) }, { slug: "beta", view: view(missing) }], "https://ployz.test/");
    expect(run).toMatchObject({ conclusion: "neutral", title: "1 config change; 1 config change · TOKEN not set" });
  });

  it("links a merged pull request to its Destination's Versions once its PR Environment is gone", () => {
    const stuck: OnMergeCheck = {
      conclusion: "neutral", title: "Didn't land in production", summary: "- production: why",
      merged_into: { project: "shop", environment: "production" },
    };
    const run = storePrCheckRun([{ slug: "acme", view: { ...view(stuck), environments: [] } }], "https://ployz.test/");
    expect(run?.detailsUrl).toBe("https://ployz.test/cloud/acme/shop/production/deployments/versions");
    expect(run?.summary).toContain("[View in Ployz](https://ployz.test/cloud/acme/shop/production/deployments/versions)");
  });

  it("keeps a long summary within GitHub's limit, whole lines then how many more, still linking Ployz", () => {
    const line = "- `web · A`: 1 → 2";
    const huge: OnMergeCheck = { ...lands, summary: Array.from({ length: 5000 }, () => line).join("\n") };
    const summary = storePrCheckRun([{ slug: "acme", view: view(huge) }], "https://ployz.test/")?.summary ?? "";
    expect(new TextEncoder().encode(summary).length).toBeLessThanOrEqual(CHECK_SUMMARY_LIMIT);
    const lines = summary.split("\n");
    const kept = lines.indexOf(lines.find((each) => each !== line) ?? "");
    expect(lines.slice(kept)).toEqual([`…and ${5000 - kept} more lines`, "", "[View in Ployz](https://ployz.test/cloud/acme/shop/pr-146?panel=on-merge)"]);
  });

  it("posts nothing when no Store has a word", () => {
    expect(storePrCheckRun([{ slug: "acme", view: view(null) }], "https://ployz.test/")).toBeNull();
    expect(storePrCheckRun([], "https://ployz.test/")).toBeNull();
  });
});
