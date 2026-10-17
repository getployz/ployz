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

export const grantLabel = (grant: TokenGrant): string => {
  if (grant.permissions === null) return "Full as you";
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
