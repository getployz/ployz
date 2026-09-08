import "@tanstack/react-start/server-only";

type OrganizationTable = {
  scope: "organization";
  whereColumn: "organization_id";
  columns?: readonly string[];
};

type UserTable = {
  scope: "user";
  whereColumn: "user_id";
  columns?: readonly string[];
};

export type PloyzTable = OrganizationTable | UserTable;

/**
 * The complete set of browser-safe tables Electric may expose. Most tables
 * are safe in full; sensitive tables must declare the exact metadata columns
 * that the proxy is allowed to reveal.
 */
export const PLOYZ_TABLES = {
  project: { scope: "organization", whereColumn: "organization_id" },
  environment: { scope: "organization", whereColumn: "organization_id" },
  service: { scope: "organization", whereColumn: "organization_id" },
  resource_lineage: { scope: "organization", whereColumn: "organization_id" },
  environment_resource: {
    scope: "organization",
    whereColumn: "organization_id",
  },
  environment_canvas_node_position: {
    scope: "organization",
    whereColumn: "organization_id",
  },
  environment_deployment: {
    scope: "organization",
    whereColumn: "organization_id",
  },
  environment_saved_state_snapshot: {
    scope: "organization",
    whereColumn: "organization_id",
    columns: ["id", "organization_id", "environment_id"],
  },
  environment_node_config_snapshot: {
    scope: "organization",
    whereColumn: "organization_id",
  },
  environment_node_introduction: {
    scope: "organization",
    whereColumn: "organization_id",
  },
  volume_remove_attempt: {
    scope: "organization",
    whereColumn: "organization_id",
  },
  github_repository_cache: { scope: "user", whereColumn: "user_id" },
} as const satisfies Record<string, PloyzTable>;

export type PloyzTableName = keyof typeof PLOYZ_TABLES;
export type OrganizationTableName = {
  [Name in PloyzTableName]: (typeof PLOYZ_TABLES)[Name]["scope"] extends "organization"
    ? Name
    : never;
}[PloyzTableName];

function isPloyzTableName(name: string): name is PloyzTableName {
  return name in PLOYZ_TABLES;
}

export function getPloyzTable(name: string): PloyzTable | null {
  return isPloyzTableName(name) ? PLOYZ_TABLES[name] : null;
}
