import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { PreviewCall } from "./store-pr-preview";
import { PR_CHECK_NAME } from "./store-pull-requests";

/** Installations this worker has told the log lack Pull requests: write, so each is told once. */
const told = new Set<number>();

/**
 * The fallback while an installation hasn't accepted Pull requests: write, so the App can't comment: the comment's
 * calls go in a neutral Ployz check instead, as its title and where its Details lead. Null when there is nothing to post.
 */
export const previewInCheck = Effect.fn("StorePullRequest.previewInCheck")(function* (
  installationId: number, calls: readonly PreviewCall[], head: string | null,
) {
  if (!told.has(installationId)) {
    told.add(installationId);
    yield* Effect.logWarning("The preview link goes in the PR check: the installation lacks Pull requests: write.", { installationId });
  }
  const detailsUrl = calls.flatMap((each) => each.url === null ? [] : [each.url])[0];
  if (detailsUrl === undefined || head === null) return null;
  return {
    headSha: head, name: PR_CHECK_NAME, conclusion: "neutral" as const, detailsUrl,
    title: calls.map((each) => each.text).join(" · "), summary: calls.map((each) => each.markdown).join("\n"),
  };
});
