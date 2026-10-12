import { Schema } from "effect";

export const SetSupportAccessInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  allowed: Schema.Boolean,
});
export type SetSupportAccessInput = typeof SetSupportAccessInput.Type;

/** The Org Store's view of the support access setting, keyed by Organization ID. */
export type SupportAccessRow = { readonly id: string; readonly allowed: boolean };
