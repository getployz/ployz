import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { PreviewCall } from "./store-pr-preview";
import { CHECK_SUMMARY_LIMIT, PR_CHECK_NAME, type storePrCheckRun } from "./store-pull-requests";

type CheckRun = NonNullable<ReturnType<typeof storePrCheckRun>>;

/** Installations this worker has told the log lack Pull requests: write, so each is told once. */
const told = new Set<number>();

const bytes = (text: string) => new TextEncoder().encode(text).length;

/**
 * The fallback while an installation hasn't accepted Pull requests: write, so the App can't comment: the comment's
 * calls go in the Ployz check instead, as its title and where its Details lead, the merge's own words kept below them.
 * A pull request with no check of its own gets a neutral one. Null when there is nothing to post.
 */
export const previewInCheck = Effect.fn("StorePullRequest.previewInCheck")(function* (
  installationId: number, run: CheckRun | null, calls: readonly PreviewCall[], head: string | null,
) {
  if (!told.has(installationId)) {
    told.add(installationId);
    yield* Effect.logWarning("The preview link goes in the PR check: the installation lacks Pull requests: write.", { installationId });
  }
  const detailsUrl = calls.find((each) => each.url !== null)?.url ?? run?.detailsUrl;
  if (calls.length === 0 || detailsUrl === undefined) return run;
  const title = calls.map((each) => each.text).join(" · ");
  const lines = calls.map((each) => each.markdown).join("\n");
  if (run === null) {
    return head === null ? null : { headSha: head, name: PR_CHECK_NAME, conclusion: "neutral" as const, detailsUrl, title, summary: lines };
  }
  const summary = `${lines}\n\n${run.summary}`;
  return {
    ...run, title, detailsUrl,
    summary: bytes(summary) <= CHECK_SUMMARY_LIMIT ? summary : `${lines}\n\n[View in Ployz](${run.detailsUrl})`,
  };
});
