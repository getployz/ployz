import { PERMISSIONS, type Permission } from "#/modules/organization/permissions";

/**
 * What an Organization Token may do, on top of its maker's role. `permissions` null is Full as you: whatever the
 * maker's role holds, now and after it changes. `environments` null writes any Environment; a list writes only the
 * Environments it names, in any Project.
 */
export type TokenGrant = {
  readonly permissions: ReadonlyArray<Permission> | null;
  readonly environments: ReadonlyArray<string> | null;
};

export const TOKEN_PRESETS = ["read", "deploy", "full"] as const;

/** A preset to mint from. Deploy names the one Environment it deploys to. */
export type TokenPreset = { readonly preset: "read" | "full" } | { readonly preset: "deploy"; readonly environment: string };

export const FULL_GRANT: TokenGrant = { permissions: null, environments: null };

const READ_PERMISSIONS = PERMISSIONS.filter((permission) => permission.endsWith(":read"));
const DEPLOY_PERMISSIONS = PERMISSIONS.filter((permission) => permission.endsWith(":read") || permission === "changes:stage" || permission === "changes:deploy");

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

/** How Tokens names a grant: the preset it was made from. */
export const grantLabel = (grant: TokenGrant): string => {
  if (grant.permissions === null) return "Full as you";
  if (same(grant.permissions, READ_PERMISSIONS)) return "Read only";
  if (same(grant.permissions, DEPLOY_PERMISSIONS)) return grant.environments === null ? "Deploy" : `Deploy ${grant.environments.join(", ")}`;
  return "Custom";
};

/** `held` narrowed to what `grant` allows. */
export const granted = (held: ReadonlyArray<Permission>, grant: TokenGrant): Permission[] =>
  grant.permissions === null ? [...held] : held.filter((permission) => grant.permissions?.includes(permission));
