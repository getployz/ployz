/**
 * A person's review of an untrusted pull request's preview, as the browser approves it: this one commit of the
 * preview runs, with the settings shown. The browser drafts the review's
 * canonical body with the ployz-core code the Servers run, and the person's passkey answers it with a prompt of its own.
 * Cloud keeps and relays the answered review and can change none of it.
 */
import type { PreviewReviewView, Review, ReviewDraft } from "@ployz/sdk";
import { previewReviewBody } from "@ployz/sdk/authority";
import { PasskeyRefused, passkeyAnswer, statementChallenge } from "#/modules/approval-session/passkey-statement";

/** Each Server must take a review a passkey answered before one goes to it. */
export const REVIEW_CAPABILITIES = ["ployz.preview.review.v1"] as const;

/** How long a review lasts. Servers cap it at `validity::MAX_LIFETIME_MILLIS`. */
const REVIEW_MS = 15 * 60_000;

/**
 * `text` as ployz-core's `Shown::text` shows a relayed string: bare when it holds only letters, digits and `-_./:@+`,
 * otherwise quoted, with `"` and `\\` escaped and every character outside printable ASCII written as an escape.
 */
export function shownText(text: string): string {
  if (/^[A-Za-z0-9\-_./:@+]+$/.test(text)) return text;
  let shown = '"';
  for (const c of text) {
    if (c === '"' || c === "\\") shown += `\\${c}`;
    else if (c === "\n") shown += "\\n";
    else if (c === "\r") shown += "\\r";
    else if (c === "\t") shown += "\\t";
    else if (c >= " " && c <= "~") shown += c;
    else shown += `\\u{${(c.codePointAt(0) ?? 0).toString(16)}}`;
  }
  return `${shown}"`;
}

/** Why a review couldn't be made, in words the dialog shows as they are. */
export class ReviewRefused extends Error {}

/** The review of `view`'s head a person approves now, for the Cluster whose Sealing public key is `cluster`. */
export function draftReview(view: PreviewReviewView, cluster: string, now = Date.now()): ReviewDraft {
  try {
    return previewReviewBody(view, { cluster, issued_at: now, expires_at: now + REVIEW_MS });
  } catch (error) {
    throw new ReviewRefused(error instanceof Error && error.message ? error.message : "This browser couldn't draft the review. Try again.");
  }
}

/** `draft`, answered by the person's passkey with a prompt of its own, ready to send with the approval. */
export async function answerReview(draft: ReviewDraft): Promise<Review> {
  try {
    return { passkey: { body: draft.body, assertion: await passkeyAnswer(await statementChallenge(draft.body, "workflow")) } };
  } catch (error) {
    if (error instanceof PasskeyRefused) throw new ReviewRefused(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new ReviewRefused("Your passkey didn't answer. Try again.");
    throw new ReviewRefused("Your browser couldn't use a passkey here.");
  }
}
