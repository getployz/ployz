import type { ConfigCommand } from "@ployz/sdk";

type Absent = { readonly absent: string; readonly quotedFrom: string };
type Gap = { readonly gap: string };
type Missing = Absent | Gap;

type StoreCommand = {
  readonly cli: { readonly paths: readonly [string, ...string[]] } | Missing;
  readonly agent: { readonly tools: readonly [string, ...string[]] } | Missing;
  readonly ui: { readonly sender: string } | Missing;
};

const ENV = "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug";
const BRANCHES = { absent: "Not here: branches", quotedFrom: "core/crates/ployz-store/src/tools/mod.rs" } as const;

export const STORE_COMMANDS = {
  create_project: {
    cli: { paths: ["project new", "up"] },
    agent: { tools: ["project_create"] },
    ui: { sender: "components/service-create-command.tsx" },
  },
  rename_project: {
    cli: { paths: ["project rename"] },
    agent: { gap: "No tool renames a Project." },
    ui: { sender: `${ENV}/-components/store-settings.tsx` },
  },
  create_environment: {
    cli: { paths: ["env new"] },
    agent: { absent: "It doesn't create environments.", quotedFrom: "docs/user/account/agent-sidebar.md" },
    ui: { sender: `${ENV}/-components/create-environment-dialog.tsx` },
  },
  set_default_environment: {
    cli: { paths: ["env default"] },
    agent: { gap: "No tool sets the default Environment." },
    ui: { sender: `${ENV}/-components/store-settings.tsx` },
  },
  set_branch_setup: {
    cli: { paths: ["env setup"] },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/store-settings.tsx` },
  },
  set_build_order: {
    cli: { paths: ["org build-order"] },
    agent: { gap: "No tool sets the build order." },
    ui: { sender: "routes/_protected/cloud/$organizationSlug/_org/-components/builds-settings.tsx" },
  },
  create_service: {
    cli: { paths: ["service add", "up"] },
    agent: { tools: ["service_add"] },
    ui: { sender: "modules/config-store/store-services.ts" },
  },
  create_git_service: {
    cli: { paths: ["service add"] },
    agent: { tools: ["service_add"] },
    ui: { sender: "modules/config-store/store-services.ts" },
  },
  rename_service: {
    cli: { paths: ["service rename"] },
    agent: { tools: ["service_rename"] },
    ui: { sender: `${ENV}/services/$serviceId/-components/StoreServiceDrawer.tsx` },
  },
  remove_service: {
    cli: { paths: ["service rm"] },
    agent: { tools: ["service_remove"] },
    ui: { sender: `${ENV}/services/$serviceId/-components/useDeleteService.ts` },
  },
  create_volume: {
    cli: { paths: ["volume add"] },
    agent: { tools: ["volume_add"] },
    ui: { sender: "modules/config-store/store-volumes.ts" },
  },
  set_volume_storage: {
    cli: { paths: ["volume set"] },
    agent: { tools: ["volume_update"] },
    ui: { sender: `${ENV}/resources/$resourceId/-components/StoreVolumeDrawer.tsx` },
  },
  set_volume_shared_writes: {
    cli: { paths: ["volume set"] },
    agent: { tools: ["volume_update"] },
    ui: { sender: `${ENV}/resources/$resourceId/-components/StoreVolumeDrawer.tsx` },
  },
  rename_volume: {
    cli: { paths: ["volume rename"] },
    agent: { tools: ["volume_update"] },
    ui: { sender: `${ENV}/resources/$resourceId/-components/StoreVolumeDrawer.tsx` },
  },
  remove_volume: {
    cli: { paths: ["volume rm"] },
    agent: { tools: ["volume_remove"] },
    ui: { sender: `${ENV}/resources/$resourceId/-components/StoreVolumeDrawer.tsx` },
  },
  create_config: {
    cli: { paths: ["config add"] },
    agent: { tools: ["config_write"] },
    ui: { sender: "modules/config-store/store-configs.ts" },
  },
  put_config_file: {
    cli: { paths: ["config put"] },
    agent: { tools: ["config_write"] },
    ui: { sender: "modules/config-store/store-configs.ts" },
  },
  remove_config_file: {
    cli: { paths: ["config rm-file"] },
    agent: { tools: ["config_remove"] },
    ui: { sender: `${ENV}/resources/$resourceId/-components/StoreConfigDrawer.tsx` },
  },
  rename_config: {
    cli: { paths: ["config rename"] },
    agent: { gap: "No tool renames a Config." },
    ui: { sender: `${ENV}/resources/$resourceId/-components/StoreConfigDrawer.tsx` },
  },
  delete_config: {
    cli: { paths: ["config rm"] },
    agent: { tools: ["config_remove"] },
    ui: { sender: `${ENV}/resources/$resourceId/-components/StoreConfigDrawer.tsx` },
  },
  attach_config: {
    cli: { paths: ["config mount"] },
    agent: { tools: ["config_write"] },
    ui: { sender: "modules/config-store/store-configs.ts" },
  },
  detach_config: {
    cli: { paths: ["config unmount"] },
    agent: { gap: "service_update unmounts a Config with an edit; no tool sends detach_config." },
    ui: { sender: "modules/config-store/StoreConfigMounts.tsx" },
  },
  edit: {
    cli: { paths: ["set", "unset", "up"] },
    agent: { tools: ["service_update"] },
    ui: { sender: "modules/config-store/store-write.ts" },
  },
  publish: {
    cli: { paths: ["publish"] },
    agent: { tools: ["publish"] },
    ui: { sender: `${ENV}/-components/canvas/useStoreChangeActions.tsx` },
  },
  stage_history: {
    cli: { paths: ["history restore", "history undo"] },
    agent: { gap: "No tool restores or undoes from history." },
    ui: { sender: `${ENV}/-components/EnvironmentHistory.tsx` },
  },
  discard: {
    cli: { paths: ["discard"] },
    agent: { tools: ["discard"] },
    ui: { sender: `${ENV}/-components/canvas/useStoreChangeActions.tsx` },
  },
  admit: {
    cli: { paths: ["deploy", "up", "deployment retry", "env shutdown"] },
    agent: { tools: ["deploy"] },
    ui: { sender: `${ENV}/-components/StoreDeploymentPage.tsx` },
  },
  start: {
    cli: { paths: ["deployment start"] },
    agent: { gap: "No tool starts a waiting Deployment." },
    ui: { sender: `${ENV}/-components/StoreDeploymentPage.tsx` },
  },
  cancel: {
    cli: { paths: ["deployment cancel", "deploy"] },
    agent: { tools: ["deployment_cancel"] },
    ui: { sender: `${ENV}/-components/StoreDeploymentPage.tsx` },
  },
  add_domain: {
    cli: { paths: ["domain add", "up"] },
    agent: { tools: ["domain_add", "domain_update"] },
    ui: { sender: `${ENV}/services/$serviceId/-components/StoreNetworkingSection.tsx` },
  },
  set_generated_domain: {
    cli: { paths: ["domain set"] },
    agent: { tools: ["domain_update"] },
    ui: { sender: `${ENV}/services/$serviceId/-components/StoreNetworkingSection.tsx` },
  },
  remove_domain: {
    cli: { paths: ["domain rm"] },
    agent: { tools: ["domain_remove"] },
    ui: { sender: `${ENV}/services/$serviceId/-components/StoreNetworkingSection.tsx` },
  },
  create_branch: {
    cli: { paths: ["env branch"] },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/new-branch/StoreNewBranchPanel.tsx` },
  },
  sync: {
    cli: { paths: ["env sync"] },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/sync/SyncDialog.tsx` },
  },
  undo_sync: {
    cli: { paths: ["env sync"] },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/sync/SyncButton.tsx` },
  },
  remove_proposal: {
    cli: { gap: "No command drops a change from a branch's sync proposal." },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/canvas/CanvasFlow.tsx` },
  },
  take: {
    cli: { paths: ["env sync"] },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/canvas/store-hints.tsx` },
  },
  copy_node: {
    cli: { paths: ["env copy"] },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/StoreLiveNodePanel.tsx` },
  },
  never_sync: {
    cli: { paths: ["env never-sync"] },
    agent: BRANCHES,
    ui: { sender: "modules/config-store/store-variables.ts" },
  },
  set_merge_value: {
    cli: { gap: "env on-merge only reads; no command sets the value a merge lands." },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/sync/OnMerge.tsx` },
  },
  keep_branch: {
    cli: { paths: ["env keep"] },
    agent: BRANCHES,
    ui: { sender: `${ENV}/-components/sync/SyncButton.tsx` },
  },
  remove: {
    cli: { paths: ["project rm", "env rm"] },
    agent: { tools: ["environment_remove", "project_remove"] },
    ui: { sender: "modules/config-store/store-workspace.ts" },
  },
  cancel_removal: {
    cli: { gap: "No command sends it; deployment cancel on the removal's Deployment stops the removal instead." },
    agent: { gap: "No tool sends it; deployment_cancel on the removal's Deployment stops the removal instead." },
    ui: { gap: "No control sends it; cancelling the removal's Deployment stops the removal instead." },
  },
  set_pr_plan: {
    cli: { paths: ["env pr"] },
    agent: { tools: ["environment_pr"] },
    ui: { sender: `${ENV}/-components/pr-environments/StorePrPlanPanel.tsx` },
  },
  batch: {
    cli: { gap: "The CLI sends one command per call." },
    agent: { tools: ["service_add", "volume_add", "config_write", "config_remove", "discard"] },
    ui: { sender: "modules/config-store/store-configs.ts" },
  },
} as const satisfies Record<ConfigCommand["command"], StoreCommand>;
