import type { EnvironmentRef } from "@ployz/sdk";

/** Names an Environment the same way however a ref spells it; the Store resolves omitted names to defaults. */
export const environmentKey = (ref: Pick<EnvironmentRef, "project" | "environment">) => `${ref.project ?? ""}/${ref.environment ?? ""}`;

export const sameEnvironmentRef = (a: EnvironmentRef, b: EnvironmentRef) =>
  (a.project ?? "") === (b.project ?? "") && (a.environment ?? "") === (b.environment ?? "");

/** Whether `listing` is the Environment `ref` names: an omitted name is the Project's default. */
export const matchesEnvironmentRef = (listing: { readonly default: boolean; readonly name: string }, ref: { readonly environment?: string | null }) =>
  ref.environment == null ? listing.default : listing.name === ref.environment;
