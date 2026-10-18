import type { DiffView } from "@ployz/sdk";
import type { ApprovalReview } from "#/modules/approvals/approvals";
import { changeGroups } from "#/modules/config-store/store-deployments";

export type ChangeLine = { key: string; mark: "+" | "~" | "−"; node: string; text: string };

/** One line per changed node or Setting in `changes`, minus what a destroyed line already says. */
function changeLines(diff: DiffView, changes: DiffView["changes"], effects: ApprovalReview["effects"]): ChangeLine[] {
  const destroyedNodes = new Set(effects.filter((effect) => effect.kind === "removes_service" || effect.kind === "deletes_volume").map((effect) => effect.node));
  const destroyedPaths = new Set(effects.map((effect) => effect.path));
  return changeGroups({ ...diff, changes }, []).flatMap((group): ChangeLine[] => {
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

/**
 * What an ask changes besides what it destroys: `asked` is the asker's own unsaved edit, which only a Save or Deploy
 * saves, and `others` everything else it deploys. A Sync or Take leaves the target's unsaved edit alone.
 */
export function reviewedChanges(command: string, review: ApprovalReview) {
  if (!("diff" in review)) return { asked: [], others: [] };
  const { effects, diff } = review;
  const saves = command === "publish" || command === "admit";
  const asked = saves ? changeLines(diff, diff.draft_changes ?? [], effects) : [];
  const keys = new Set(asked.map((line) => line.key));
  return { asked, others: changeLines(diff, diff.changes, effects).filter((line) => !keys.has(line.key)) };
}
