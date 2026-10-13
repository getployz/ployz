import type { HistoryAction, JsonValue, NotSetVariable } from "@ployz/sdk";
import { Schema } from "effect";
import { listNames } from "#/lib/plural";

/** The toast after Undo or Roll back brought sealed variables back without their values. */
export function notSetToast(action: HistoryAction, notSet: readonly NotSetVariable[]) {
  const keys = [...new Set(notSet.map((variable) => variable.key))];
  if (!keys.length) return undefined;
  const one = keys.length === 1;
  return {
    title: action === "undo" ? "Undo staged" : "Roll back staged",
    description: `${listNames(keys)} came back Not set. Deploys leave ${one ? "it" : "them"} out until you set ${one ? "it" : "them"}.`,
  };
}

const isSealed = Schema.is(Schema.Struct({ secret: Schema.Boolean }));

/**
 * Variables a save left sealed. History keeps a sealed value as it is now, so undoing these changes nothing.
 * A change linked to others, such as the new name of a renamed secret, is left out: its Undo reverses them too.
 */
export function sealedOnlyPaths(changes: readonly { settings: readonly { path: string; before: JsonValue; after: JsonValue }[] }[],
  linked: { readonly [path: string]: number }) {
  return new Set(changes.flatMap((change) => change.settings
    .filter((row) => isSealed(row.after) && (linked[row.path] ?? 1) === 1).map((row) => row.path)));
}
