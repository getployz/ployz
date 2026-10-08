import type { DestructiveEffect, DestructiveKind } from "@ployz/sdk";
import type { ApprovalReview } from "#/modules/approvals/approvals";
import { changeGroups } from "#/modules/config-store/store-deployments";

const destroys = {
  removes_service: { verb: "Removes service", after: "" },
  deletes_volume: { verb: "Deletes volume", after: ". Not recoverable." },
  detaches_volume: { verb: "Detaches volume", after: "" },
  removes_domain: { verb: "Removes domain", after: "" },
  removes_server: { verb: "Removes server", after: "" },
} satisfies Record<DestructiveKind, { verb: string; after: string }>;

/** One thing the plan destroys, worded as the card leads with it. */
export function destroyedLine(effect: DestructiveEffect) {
  const { verb, after } = destroys[effect.kind];
  const mount = effect.kind === "detaches_volume" ? effect.path.split(".mounts.") : [];
  return { verb, name: effect.name ?? effect.node, after: mount.length === 2 ? ` from ${mount[0]}${after}` : after };
}

const capitalized = (word: string) => `${word.charAt(0).toUpperCase()}${word.slice(1)}`;

/** What the card asks about, as verb and name: "Deploy production", "Publish production" or "Drain fra-1". */
export function approvalSubject(command: string, review: ApprovalReview) {
  if ("operation" in review) return { verb: capitalized(review.operation.verb), name: review.operation.name };
  return { verb: command === "publish" ? "Publish" : "Deploy", name: review.diff.environment.name };
}

export type OtherChange = { key: string; mark: "+" | "~" | "−"; node: string; text: string };

/** Everything else a Publish or Deploy changes, one line per changed node or Setting, minus what a destroyed line already says. */
export function otherChanges(review: ApprovalReview): OtherChange[] {
  if (!("diff" in review)) return [];
  const { effects, diff } = review;
  const destroyedNodes = new Set(effects.filter((effect) => effect.kind === "removes_service" || effect.kind === "deletes_volume").map((effect) => effect.node));
  const destroyedPaths = new Set(effects.map((effect) => effect.path));
  return changeGroups(diff, []).flatMap((group): OtherChange[] => {
    if (group.lifecycle === "delete") {
      return destroyedNodes.has(group.nodeId) ? [] : [{ key: group.nodeId, mark: "−", node: group.nodeName, text: `removed ${group.nodeType}` }];
    }
    if (group.lifecycle === "create") return [{ key: group.nodeId, mark: "+", node: group.nodeName, text: `new ${group.nodeType}` }];
    return group.rows.filter((row) => !destroyedPaths.has(row.path)).map((row) => ({
      key: row.changeKey,
      mark: row.kind === "add" ? "+" : row.kind === "remove" ? "−" : "~",
      node: group.nodeName,
      text: row.kind === "update" ? `${row.name} ${row.currentValue} → ${row.newValue}` : `${row.name} ${row.kind === "add" ? row.newValue : row.currentValue}`,
    }));
  });
}
