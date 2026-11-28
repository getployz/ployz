import type { ConfigQuery } from "@ployz/sdk";
import * as EffectRecord from "effect/Record";

/**
 * The Config Store's organization-owned tables, keyed by the scope whose views a change invalidates. The Store
 * creates them when Cloud first opens it, so Cloud attaches their triggers then (see store-sdk.server.ts).
 * The Organization column is text there, because the Store's SQL also runs on SQLite.
 *
 * A table's `family` is the change name its changes send. A family's first table is its key table, and every other
 * table logs that table's key, so a logged key names the Project, Environment or Deployment whose views changed. A
 * created or deleted row is one a view gains or drops. Which views each family refreshes is `storeViewRefreshedBy`.
 */
export const storeChangeSources = {
  config_project: { key: ["id"], family: "store_project" },
  config_environment: { key: ["id"], family: "store_environment" },
  config_node_introduction: { key: ["environment_id"], family: "store_environment" },
  config_saved: { key: ["environment_id"], family: "store_environment" },
  config_namespace: { key: ["environment_id"], family: "store_environment" },
  config_deployment: { key: ["id"], family: "store_deployment" },
  config_build: { key: ["deployment_id"], family: "store_deployment" },
  config_deployment_row: { key: ["deployment_id"], family: "store_deployment" },
  config_applied: { key: ["environment_id"], family: "store_environment" },
  config_registry_login: { key: ["environment_id"], family: "store_environment" },
  config_service_policy: { key: ["environment_id"], family: "store_environment" },
  config_environment_branch: { key: ["environment_id"], family: "store_environment" },
  config_sync_base: { key: ["environment_id"], family: "store_environment" },
  config_never_sync: { key: ["environment_id"], family: "store_environment" },
  // ponytail: logs the receiver only; another tab on the sender sees what it synced once the sender changes too.
  config_sync_arrival: { key: ["environment_id"], family: "store_environment" },
  config_proposal: { key: ["environment_id"], family: "store_environment" },
  config_merge_value: { key: ["environment_id"], family: "store_environment" },
  // No family: Cloud never reads a merge's deferred deploy; the next push to the branch does.
  config_merge_deploy: { key: ["environment_id"] },
  // No family: a receipt outlives its Environments and no view reads it.
  config_sync_receipt: { key: ["environment_id"] },
  config_build_order: { key: ["organization_id"], family: "store_organization" },
  config_region_name: { key: ["organization_id"], family: "store_region" },
  config_pr_plan: { key: ["project_id"], family: "store_project" },
  config_pr_environment: { key: ["environment_id"], family: "store_environment" },
  config_pull_request: { key: ["repository_id", "number"], family: "store_pull_request" },
  // Keyed by Organization: a suite names a commit, and its verdict shows on every Deployment waiting for it, which
  // polls nothing while it waits.
  config_check_suite: { key: ["organization_id"], family: "store_check_suite" },
} as const satisfies Record<string, { key: readonly string[]; family?: string }>;

export type StoreTable = keyof typeof storeChangeSources;

type FamilyOf<T> = T extends { family: infer F } ? F : never;
export type StoreViewName = FamilyOf<(typeof storeChangeSources)[StoreTable]>;

export const storeViewNames = ["store_project", "store_environment", "store_deployment", "store_organization", "store_pull_request", "store_check_suite", "store_region"] as const satisfies readonly StoreViewName[];
// Compile-time: every family a table names is listed above.
const everyFamilyListed: Exclude<StoreViewName, (typeof storeViewNames)[number]> extends never ? true : never = true;
void everyFamilyListed;

/** The tables whose changes a family names, key table first. */
export const storeTablesOf = (family: string) =>
  EffectRecord.keys(storeChangeSources).filter((table) => {
    const source = storeChangeSources[table];
    return "family" in source && source.family === family;
  });

/**
 * The table families that refresh each query kind. A new Query kind must say which tables back it,
 * or the Organization change stream never refreshes its views.
 */
export const storeViewRefreshedBy = {
  environment: ["store_environment"],
  diff: ["store_environment", "store_deployment"],
  // Live and Failed follow Deployments; a check suite's verdict shows on one waiting for CI.
  history: ["store_environment", "store_deployment", "store_check_suite"],
  history_preview: ["store_environment", "store_deployment", "store_check_suite"],
  plan: ["store_environment", "store_deployment"],
  // The rule a Device or a passkey signed, beside the Saved State it approves.
  rule: ["store_environment"],
  // A Service's lifecycle comes from the review, which compares against what's deployed.
  service: ["store_environment", "store_deployment"],
  services: ["store_environment", "store_deployment"],
  // A check suite's verdict shows on a Deployment waiting for CI, which polls nothing while it waits.
  deployments: ["store_deployment", "store_check_suite"],
  deployment: ["store_deployment", "store_check_suite"],
  numbered_deployment: ["store_deployment", "store_check_suite"],
  build_log: ["store_deployment"],
  // Admission fixes an Environment's Namespace.
  namespace: ["store_environment", "store_deployment"],
  namespaces: ["store_environment", "store_deployment"],
  sealed_names: ["store_environment"],
  // Where a CLI write's secrets land: the Environment's Services, by ID.
  sealing_targets: ["store_environment"],
  // Whether a domain is deployed follows Deployments; its certificate and DNS are Cloud's observations, which
  // change with no Store write, so a domains view also polls while one is on its way (`DOMAIN_POLL_MS`).
  domains: ["store_environment", "store_deployment"],
  domain: ["store_environment", "store_deployment"],
  // Whether a Volume is deployed, and what a Deploy removes, come from Applied State.
  volumes: ["store_environment", "store_deployment"],
  volume: ["store_environment", "store_deployment"],
  configs: ["store_environment", "store_deployment"],
  config: ["store_environment", "store_deployment"],
  removals: ["store_environment", "store_deployment"],
  removed: ["store_environment", "store_deployment"],
  // A Branch's Live Nodes follow what its Parent and ancestors run; a PR Environment counts toward its pull
  // request's Destination.
  branch: ["store_environment", "store_deployment", "store_pull_request", "store_check_suite"],
  // A plan reads the Environment's Working State and what it and its ancestors run.
  branch_plan: ["store_environment", "store_deployment"],
  build_order: ["store_organization"],
  // Region names; the regions Servers are in come with the query, from the Runtime.
  regions: ["store_region"],
  new_region: ["store_region"],
  // A Sync compares two Environments over what they last shared.
  sync: ["store_environment", "store_deployment"],
  // The Project names its Default Environment; a removal is a Deployment.
  environments: ["store_project", "store_environment", "store_deployment"],
  projects: ["store_project", "store_environment"],
  // Removals the Store still owes: Environments marked for removal and their Deployments.
  due: ["store_environment", "store_deployment"],
  consented: ["store_environment", "store_deployment"],
  // Which uploads Cloud holds still build: Deployments and each Environment's Saved State.
  uploads: ["store_environment", "store_deployment"],
  // Plans list the repositories Working States deploy from, and name nodes of the start-from Environment.
  pr_plans: ["store_project", "store_environment"],
  // Each pull request's preview: its facts, the plan's rule and cap, and each preview's Deployments.
  previews: ["store_project", "store_environment", "store_deployment", "store_pull_request"],
  // Open previews are PR Environments in preview Namespaces; a plan keeps the preview rule.
  open_previews: ["store_project", "store_environment"],
  // PR Environments, their Deployments, each Destination's Working State, and the facts GitHub last told Cloud.
  pull_request: ["store_environment", "store_deployment", "store_pull_request", "store_check_suite"],
  // What a merge lands: both Environments' Working and Saved, the Edits, and the pull request's facts.
  on_merge: ["store_environment", "store_pull_request"],
  // Signed versions arrive with a Deploy and are never rewritten.
  versions: ["store_deployment"],
  // A parent is the newest applied Deployment's signed version, with the Saved spec it ran.
  parent: ["store_environment", "store_deployment"],
  // What a Service runs now is the newest applied Deployment's spec, with the signed versions kept for it.
  running: ["store_environment", "store_deployment"],
} satisfies Record<ConfigQuery["query"], readonly StoreViewName[]>;
