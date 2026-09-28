import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { ChangeRow } from "./branch-review";
import { branchNews, newsLabel } from "./branch-news";
import type { BranchReviewView } from "./use-branch-review";

type Review = Parameters<typeof branchNews>[0];
const row = asTestDouble<ChangeRow>()({ key: "web-lineage:source.image" });
const branch = (save: number, updates: number): Review => ({ pullRequest: null, goesTo: [], save: Array<ChangeRow>(save).fill(row), updates });
const landing = (id: string, rows: number, saved: boolean) => asTestDouble<BranchReviewView["goesTo"][number]>()({
  destination: { id, name: id, namespace: id }, rows: Array<ChangeRow>(rows).fill(row), saved: saved ? { id: `s-${id}` } : null,
});
const pr = (closed: boolean, updates: number, ...goesTo: ReturnType<typeof landing>[]) => asTestDouble<Review>()({
  pullRequest: { closed }, goesTo, save: [], updates,
});
const kinds = (news: ReturnType<typeof branchNews>) => news.map((item) => item.kind);

it("puts the most pressing first: a failed shutdown, changes to save, updates, Off, closing, saved; else up to date", () => {
  expect(kinds(branchNews(branch(0, 0), null, null))).toEqual(["up_to_date"]);
  expect(kinds(branchNews(branch(2, 1), null, 2))).toEqual(["save", "update", "closing"]);
  expect(kinds(branchNews(pr(false, 1, landing("prod", 2, false), landing("eu", 1, true)), "off", null)))
    .toEqual(["save", "update", "off", "saved"]);
  expect(kinds(branchNews(pr(false, 0, landing("prod", 2, false)), "failed", null))).toEqual(["shutdown_failed", "save"]);
  expect(kinds(branchNews(pr(false, 0, landing("prod", 0, false)), "running", null))).toEqual(["shutting_down"]);
  // A closed pull request takes no more saves.
  expect(kinds(branchNews(pr(true, 0, landing("prod", 2, true)), null, null))).toEqual(["up_to_date"]);
});

it("labels the Branch button by the first, counting changes to save over every Destination", () => {
  expect(newsLabel(branchNews(branch(0, 0), null, null))).toBe("Up to date");
  expect(newsLabel(branchNews(branch(1, 3), null, null))).toBe("1 to save");
  expect(newsLabel(branchNews(pr(false, 0, landing("prod", 2, false), landing("eu", 3, false)), null, null))).toBe("5 to save");
  expect(newsLabel(branchNews(branch(0, 1), "off", null))).toBe("1 update");
  expect(newsLabel(branchNews(branch(0, 0), "off", 2))).toBe("Off");
  expect(newsLabel(branchNews(branch(0, 0), null, 1))).toBe("Closes in 1 day");
  expect(newsLabel(branchNews(pr(false, 0, landing("prod", 2, true)), null, null))).toBe("Saved");
  expect(newsLabel(branchNews(pr(false, 0, landing("prod", 2, false)), "failed", null))).toBe("Shutdown failed");
});
