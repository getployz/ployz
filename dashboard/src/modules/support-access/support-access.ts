import { Schema } from "effect";

/** What Ployz support may read inside an Organization beyond its shape. Each is off until a member turns it on. */
export type SupportShows = { readonly variables: boolean; readonly configs: boolean; readonly logs: boolean };

export const SUPPORT_SHOWS_NONE: SupportShows = { variables: false, configs: false, logs: false };

/** One switch under Settings > Support. Turning `allowed` off turns every other switch off with it. */
export const SupportAccessChange = Schema.Struct({
  setting: Schema.Literals(["allowed", "variables", "configs", "logs"]),
  on: Schema.Boolean,
});
export type SupportAccessChange = typeof SupportAccessChange.Type;

export const SetSupportAccessInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  change: SupportAccessChange,
});
export type SetSupportAccessInput = typeof SetSupportAccessInput.Type;

/** The Org Store's view of the support access setting, keyed by Organization ID. */
export type SupportAccessRow = { readonly id: string; readonly allowed: boolean } & SupportShows;

/** `row` after `change`, as the server applies it: off for support access is off for everything it shows. */
export const applySupportAccessChange = (row: SupportAccessRow, change: SupportAccessChange): SupportAccessRow => {
  if (change.setting === "allowed") return change.on ? { ...row, allowed: true } : { id: row.id, allowed: false, ...SUPPORT_SHOWS_NONE };
  return row.allowed ? { ...row, [change.setting]: change.on } : row;
};
