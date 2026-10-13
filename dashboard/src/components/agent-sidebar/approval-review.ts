import type { ApprovalReview } from "#/modules/approvals/approvals";
import { changeGroups } from "#/modules/config-store/store-deployments";

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
