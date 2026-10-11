import type { DestructiveEffect, DestructiveKind } from "@ployz/sdk";
import type { ApprovalReview } from "#/modules/approvals/approvals";
import { changeGroups } from "#/modules/config-store/store-deployments";

const destroys = {
  removes_service: { verb: "Removes", noun: "service", after: "" },
  deletes_volume: { verb: "Deletes", noun: "volume", after: ". Not recoverable." },
  detaches_volume: { verb: "Detaches", noun: "volume", after: "" },
  removes_domain: { verb: "Removes", noun: "domain", after: "" },
  removes_server: { verb: "Removes", noun: "server", after: "" },
} satisfies Record<DestructiveKind, { verb: string; noun: string; after: string }>;

/** One thing the plan destroys, worded as the card leads with it: by name, or as "a volume" when it has none. */
export function destroyedLine(effect: DestructiveEffect) {
  const { verb, noun, after } = destroys[effect.kind];
  const mount = effect.kind === "detaches_volume" ? effect.path.split(".mounts.") : [];
  const tail = mount.length === 2 ? ` from ${mount[0]}${after}` : after;
  return effect.name === undefined ? { verb: `${verb} a ${noun}`, name: null, after: tail } : { verb: `${verb} ${noun}`, name: effect.name, after: tail };
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
