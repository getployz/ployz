import { Schema } from "effect";
import { PERMISSIONS, type Permission, reads } from "#/modules/organization/permissions";

export type TokenGrant = {
  readonly permissions: ReadonlyArray<Permission> | null;
  readonly environments: ReadonlyArray<string> | null;
};

export const TOKEN_PRESETS = ["read", "deploy", "full"] as const;

export type TokenPreset = { readonly preset: "read" | "full" } | { readonly preset: "deploy"; readonly environment: string };

/** The Store's Environment name: a DNS label. */
export const EnvironmentName = Schema.String.check(Schema.isPattern(/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/));

/** The preset a token request names with its Environment, or why they don't go together. */
export const presetOf = (
  input: { readonly preset?: TokenPreset["preset"] | undefined; readonly environment?: string | undefined },
): { readonly ok: true; readonly preset: TokenPreset } | { readonly ok: false; readonly message: string } => {
  const preset = input.preset ?? "full";
  if (preset === "deploy") {
    // Any other name is looked up, so one that isn't a DNS label hears that no Environment has it.
    return input.environment === undefined || input.environment === ""
      ? { ok: false, message: "A Deploy token needs an Environment; pass `--env`." }
      : { ok: true, preset: { preset, environment: input.environment } };
  }
  return input.environment === undefined
    ? { ok: true, preset: { preset } }
    : { ok: false, message: "Only a Deploy token takes an Environment." };
};

export const FULL_GRANT: TokenGrant = { permissions: null, environments: null };

const READ_PERMISSIONS = PERMISSIONS.filter(reads);
const DEPLOY_PERMISSIONS = PERMISSIONS.filter((permission) => reads(permission) || permission === "changes:stage" || permission === "changes:deploy");

export const presetGrant = (choice: TokenPreset): TokenGrant => {
  switch (choice.preset) {
    case "read":
      return { permissions: READ_PERMISSIONS, environments: null };
    case "deploy":
      return { permissions: DEPLOY_PERMISSIONS, environments: [choice.environment] };
    case "full":
      return FULL_GRANT;
  }
};

const same = (left: ReadonlyArray<string>, right: ReadonlyArray<string>) =>
  left.length === right.length && left.every((value) => right.includes(value));

/** `as` names who a full token acts as: "you" for the viewer's own, its owner for a teammate's. */
export const grantLabel = (grant: TokenGrant, as = "you"): string => {
  if (grant.permissions === null) return `Full as ${as}`;
  if (same(grant.permissions, READ_PERMISSIONS)) return "Read only";
  if (same(grant.permissions, DEPLOY_PERMISSIONS)) return grant.environments === null ? "Deploy" : `Deploy ${grant.environments.join(", ")}`;
  return "Custom";
};

/** A token's refusal to write outside its Environments, worded as the Store words it. */
export const outsideEnvironments = (environments: ReadonlyArray<string>) => {
  const names = [...environments].sort();
  const last = names.pop() ?? "";
  return `This token can deploy only to ${names.length === 0 ? last : `${names.join(", ")} and ${last}`}.`;
};

export const granted = (held: ReadonlyArray<Permission>, grant: TokenGrant): Permission[] =>
  grant.permissions === null ? [...held] : held.filter((permission) => grant.permissions?.includes(permission));
