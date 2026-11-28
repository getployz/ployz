import { rollbackConfirm } from "#/modules/config-store/store-deployments";
import { type ApprovalReview, approvalSubject, asksForAuthKey, prunes, rollsBack, shutsDownAgain } from "#/modules/approvals/approvals";
import type { ApprovalView } from "#/modules/approvals/approvals.queries";

/** The badge of an approval whose command alone says what it does. */
const BADGE = new Map([
  ["stop", "Stops service"],
  ["restart", "Restarts service"],
  ["unprotect", "Unprotects"],
  ["sync", "Syncs"],
  ["take", "Takes"],
  ["cancel", "Cancels deploy"],
  ["move", "Moves volume"],
  ["restore", "Restores volume"],
  ["resize", "Resizes volume"],
  ["set_cluster_name", "Renames cluster"],
]);

const counted = (count: number, one: string, many: string) => `${count} ${count === 1 ? one : many}`;

/** The card's badge: a Save deletes nothing until the next Deploy, and a Deploy that only deletes Volumes counts them. An agent's Deploy that destroys nothing still asks. */
export function approvalBadge(command: string, review: ApprovalReview) {
  const { effects } = review;
  if (command === "publish") return "Deletes on deploy";
  if (command === "retry" && !shutsDownAgain(review)) return rollsBack(review) ? "Rolls back" : "Redeploys";
  const named = BADGE.get(command);
  if (named !== undefined) return named;
  if (command === "set_private_access") return asksForAuthKey(review) ? "Joins a tailnet" : "Removes routers";
  if (command === "set_ingress_doors") return effects.length === 0 ? "Switches web traffic" : `Moves ${counted(effects.length, "address", "addresses")}`;
  if (command === "release" && effects.length === 0) return "Releases volume";
  if (effects.length === 0) return prunes(review) ? "Removes other services" : "Deploys";
  if (effects.every((effect) => effect.kind === "deletes_volume")) return `Deletes ${counted(effects.length, "volume", "volumes")}`;
  return `Destroys ${counted(effects.length, "thing", "things")}`;
}

/** What a settled approval's trace line says happens next. */
export function approvedLine(approval: ApprovalView) {
  if (approval.command === "publish") return "Saves without deploying.";
  if (approval.command === "set_private_access") return asksForAuthKey(approval.review) ? "Check private access in Integrations." : "Private access turns off.";
  return `${approvalSubject(approval.command, approval.review).verb} goes ahead.`;
}

/** What a redeploy's card says it ships: "Back to #4. Undoes #5 and #6.", or "Ships #4 again."; null for anything else. */
export function redeployLine(command: string, review: ApprovalReview) {
  if (command !== "retry" || !("diff" in review) || !review.redeploy) return null;
  const { deployment, rolls_back } = review.redeploy;
  const rollback = rollbackConfirm({ number: deployment, rolls_back });
  return rollback ? `Back to #${deployment}. ${rollback.body}.` : `Ships #${deployment} again.`;
}
