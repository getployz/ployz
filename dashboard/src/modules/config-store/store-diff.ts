import type { DiffView, NodeChange } from "@ployz/sdk";
import { Schema } from "effect";
import { holds } from "#/modules/support-access/support-view";

const isSealed = Schema.is(Schema.Struct({ secret: Schema.Boolean }));

/**
 * `diff` with each variable it seals shown sealed before too: saving the seal removes the earlier plain value from
 * History, so an approval kept after it runs keeps none either. Cloud checks only a refusal's version and Environment,
 * so a review without changes passes through as it is.
 */
export function withoutSealedBefore<T extends DiffView>(diff: T): T {
  const node = (change: NodeChange): NodeChange => ({
    ...change,
    settings: change.settings.map((row) =>
      holds(row.row, row.path) === "variable" && isSealed(row.after) && row.before !== null && !isSealed(row.before) ? { ...row, before: { secret: true } } : row),
  });
  const nodes = (changes: NodeChange[] | undefined) => changes?.map(node);
  const shown = { ...diff };
  const changes = nodes(diff.changes);
  if (changes) shown.changes = changes;
  const drafts = nodes(diff.draft_changes);
  if (drafts) shown.draft_changes = drafts;
  return shown;
}
