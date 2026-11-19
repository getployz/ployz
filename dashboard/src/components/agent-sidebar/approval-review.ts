import type { DiffView } from "@ployz/sdk";
import type { ApprovalReview } from "#/modules/approvals/approvals";
import { changeGroups, regionName, type Homes } from "#/modules/config-store/store-deployments";

/** `name` is a Setting, a variable key when `variable`, or what happened to the node; `values` its machine values. */
export type ChangeLine = { key: string; mark: "+" | "~" | "−"; node: string; name: string; variable: boolean; values: string };

/** One line per changed node or Setting in `changes`, minus what a destroyed line already says. */
function changeLines(changes: DiffView["changes"], homes: Homes, effects: ApprovalReview["effects"]): ChangeLine[] {
  const destroyedNodes = new Set(effects.filter((effect) => effect.kind === "removes_service" || effect.kind === "deletes_volume").map((effect) => effect.node));
  const destroyedPaths = new Set(effects.map((effect) => effect.path));
  return changeGroups({ changes }, [], homes).flatMap((group): ChangeLine[] => {
    if (group.lifecycle === "delete") {
      return destroyedNodes.has(group.nodeId) ? [] : [{ key: group.nodeId, mark: "−", node: group.nodeName, name: `removed ${group.nodeType}`, variable: false, values: "" }];
    }
    if (group.lifecycle === "create") return [{ key: group.nodeId, mark: "+", node: group.nodeName, name: `new ${group.nodeType}`, variable: false, values: "" }];
    return group.rows.filter((row) => !destroyedPaths.has(row.path)).map((row) => ({
      key: row.changeKey,
      mark: row.kind === "add" ? "+" : row.kind === "remove" ? "−" : "~",
      node: group.nodeName,
      name: row.name,
      variable: row.variable,
      // A Config file's text is too long for a line; its name says which file.
      values: row.configFile ? "" : row.kind === "update" ? `${row.currentValue} → ${row.newValue}` : row.kind === "add" ? row.newValue : row.currentValue,
    }));
  });
}

/**
 * The Environment's region on each side of a comparison the review holds. The review doesn't say the region where
 * neither side changed it, so a bare replica count then reads without one.
 */
const homesOf = (change: DiffView["region"]): Homes => (change ? [change.before, change.after] : [null, null]);

/** The region change, which no node of `changes` holds. */
function regionLine(diff: DiffView): ChangeLine[] {
  if (!diff.region) return [];
  const { before, after } = diff.region;
  return [{ key: "@environment.region", mark: before === null ? "+" : after === null ? "−" : "~", node: diff.environment.name, name: "Region", variable: false,
    values: `${regionName(before)} → ${regionName(after)}` }];
}

/**
 * What an ask changes besides what it destroys: `region` moves every Service, `asked` is the asker's own unsaved edit,
 * which only a Save or Deploy saves, and `others` everything else it deploys. A Sync or Take leaves the target's
 * unsaved edit alone.
 */
export function reviewedChanges(command: string, review: ApprovalReview) {
  if (!("diff" in review)) return { region: [], asked: [], others: [] };
  const { effects, diff } = review;
  const saves = command === "publish" || command === "admit";
  const asked = saves ? changeLines(diff.draft_changes ?? [], homesOf(diff.draft_region), effects) : [];
  const keys = new Set(asked.map((line) => line.key));
  return { region: regionLine(diff), asked, others: changeLines(diff.changes, homesOf(diff.region), effects).filter((line) => !keys.has(line.key)) };
}
