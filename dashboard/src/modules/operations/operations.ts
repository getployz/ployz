import { Schema } from "effect";
import { EnqueueMachineRemoveInput, ForgetServerInput } from "#/modules/machines/machine-removal";
import { ForgetServersInput } from "#/modules/machines/forget-servers";
import { RemoveNamespaceInput } from "#/modules/machines/namespace-cleanup";
import { RequestServerDrainInput } from "#/modules/machines/server-drain";
import { ForgetServer, ForgetServers, RemoveServer, UpgradeServer, VOLUME_RUN_BODIES } from "#/modules/operations/cli-wire";
import { RequestServerUpgradeInput } from "#/modules/server-upgrade/server-upgrade";
import { RefreshMirrorInput } from "#/modules/volume-copies/volume-copies";
import { DeleteCopyInput, StageRestoreInput } from "#/modules/volume-placement/volume-placement";
import type { VolumeRunKind } from "#/modules/volume-run/volume-run";

/** The tool name a request key is scoped to: the same key under another tool is another request. */
export const OPERATION_TOOLS = [
  "server_drain",
  "server_upgrade",
  "server_clean",
  "server_remove",
  "server_forget",
  "volume_mirror",
  "volume_move",
  "volume_release",
  "volume_mirror_remove",
  "volume_restore",
  "volume_seed",
  "service_restart",
  "service_start",
  "service_stop",
] as const;
export type OperationTool = (typeof OPERATION_TOOLS)[number];

/** How one surface carries an option: the field (or fields, together) that holds it, or why it has none. */
type Carried = { readonly via: string | readonly [string, ...string[]] } | { readonly omitted: string };
type Absent = { readonly absent: string };
type Carries<O extends string> = { readonly [K in O]: Carried };

type Struct = Schema.Struct<Schema.Struct.Fields>;
type Surfaces<O extends string> = {
  /** `ployz` through `/api/cli`: `:id` is the route's id segment, the rest are body fields. */
  readonly cli: { readonly route: string; readonly body: Struct | null; readonly options: Carries<O> } | Absent;
  /** The agent's store tool, whose input schema is in the `agent_tools.json` snapshot. */
  readonly agent: { readonly tool: string; readonly options: Carries<O> } | Absent;
  /** The dashboard's server fn and the input it validates. `ungated` says why it skips the approval gate. */
  readonly ui: { readonly fn: string; readonly input: Struct; readonly ungated?: string; readonly options: Carries<O> } | Absent;
};

type Idempotency = OperationTool | { readonly none: string };

type Operation<F extends Schema.Struct.Fields, I extends Idempotency> = {
  readonly options: Schema.Struct<F>;
  /** `approval` asks a human before a write with effects; `none` writes straight away. */
  readonly gate: "approval" | "none";
  readonly idempotency: I;
  readonly surfaces: Surfaces<Extract<keyof F, string>>;
};

const operation = <F extends Schema.Struct.Fields, const I extends Idempotency>(definition: Operation<F, I>) => definition;

const Name = Schema.NonEmptyString;
const Names = Schema.Array(Schema.NonEmptyString);
const Seconds = Schema.Int;

const CLI_FOLLOWS = { omitted: "The CLI follows the run itself." } as const;
const UI_FOLLOWS = { omitted: "The page follows the run live." } as const;
const CLI_APPROVAL = { omitted: "The CLI asks for an approval, which shows the loss." } as const;
const AGENT_ENVIRONMENT = { via: ["project", "environment"] } as const;
const AGENT_LOSS = { via: ["accept_volume_loss", "version"] } as const;
const STAGED = { absent: "The dashboard stages it in the Volume's placement, and the deploy runs it." } as const;
const VOLUME_RUNS = "POST volumes/:id/runs";

/** Every Cloud operation, and how each surface (CLI, agent, dashboard) carries each of its options. */
export const OPERATIONS = {
  "server.remove": operation({
    options: Schema.Struct({ server: Name, acceptLoss: Names }),
    gate: "approval",
    idempotency: "server_remove",
    surfaces: {
      cli: { route: "DELETE servers/:id", body: RemoveServer, options: { server: { via: ":id" }, acceptLoss: { via: "confirm_data_loss" } } },
      agent: { tool: "server_remove", options: { server: { via: "server" }, acceptLoss: AGENT_LOSS } },
      ui: {
        fn: "enqueueMachineRemoveServerFn",
        input: EnqueueMachineRemoveInput,
        ungated: "The human confirms in the page's own dialog.",
        options: { server: { via: "machineId" }, acceptLoss: { via: "confirmDataLoss" } },
      },
    },
  }),
  "server.forget": operation({
    options: Schema.Struct({ server: Name }),
    gate: "approval",
    idempotency: "server_forget",
    surfaces: {
      cli: { route: "POST servers/:id/forget", body: ForgetServer, options: { server: { via: ":id" } } },
      agent: { tool: "server_forget", options: { server: { via: "server" } } },
      ui: { fn: "forgetServerServerFn", input: ForgetServerInput, options: { server: { via: "machineId" } } },
    },
  }),
  "server.drain": operation({
    options: Schema.Struct({ server: Name, wait: Seconds, stageOff: Schema.Boolean }),
    gate: "approval",
    idempotency: "server_drain",
    surfaces: {
      cli: {
        route: "POST servers/:id/drain",
        body: null,
        options: { server: { via: ":id" }, wait: CLI_FOLLOWS, stageOff: { omitted: "The CLI drains only; `volume move` moves a Volume off." } },
      },
      agent: {
        tool: "server_drain",
        options: { server: { via: "server" }, wait: { via: "wait_seconds" }, stageOff: { omitted: "The agent drains only; volume_move moves a Volume off." } },
      },
      ui: {
        fn: "requestServerDrainServerFn",
        input: RequestServerDrainInput,
        options: {
          server: { via: "machineId" },
          wait: UI_FOLLOWS,
          stageOff: { omitted: "Drain stages the Volumes off in a second call, stageOffServerServerFn." },
        },
      },
    },
  }),
  "server.upgrade": operation({
    options: Schema.Struct({ server: Name, behind: Schema.Boolean, channel: Name, ingressImage: Name, wait: Seconds }),
    gate: "none",
    idempotency: "server_upgrade",
    surfaces: {
      cli: {
        route: "POST servers/:id/upgrade",
        body: UpgradeServer,
        options: {
          server: { via: ":id" },
          behind: { omitted: "Cloud upgrades one Server per call; `server upgrade` names each one in order." },
          channel: { via: "channel" },
          ingressImage: { omitted: "Only an upgrade with --context pins the ingress image." },
          wait: CLI_FOLLOWS,
        },
      },
      agent: {
        tool: "server_upgrade",
        options: {
          server: { via: "server" },
          behind: { omitted: "The agent upgrades one Server per call." },
          channel: { omitted: "The agent upgrades along the Organization's Release Channel." },
          ingressImage: { omitted: "Only an upgrade with --context pins the ingress image." },
          wait: { via: "wait_seconds" },
        },
      },
      ui: {
        fn: "requestServerUpgradeServerFn",
        input: RequestServerUpgradeInput,
        options: {
          server: { via: "machineId" },
          behind: { via: "machineId" },
          channel: { omitted: "The dashboard upgrades along the Organization's Release Channel." },
          ingressImage: { omitted: "Only an upgrade with --context pins the ingress image." },
          wait: UI_FOLLOWS,
        },
      },
    },
  }),
  "namespace.clean": operation({
    options: Schema.Struct({ namespace: Name, acceptLoss: Names }),
    gate: "approval",
    idempotency: "server_clean",
    surfaces: {
      cli: { route: "POST namespaces/:id/clean", body: null, options: { namespace: { via: ":id" }, acceptLoss: CLI_APPROVAL } },
      agent: { tool: "server_clean", options: { namespace: { via: "leftover" }, acceptLoss: AGENT_LOSS } },
      ui: {
        fn: "removeNamespaceServerFn",
        input: RemoveNamespaceInput,
        ungated: "The human confirms in the page's own dialog.",
        options: { namespace: { via: "namespace" }, acceptLoss: { via: "confirmDataLoss" } },
      },
    },
  }),
  "volume.mirror": operation({
    options: Schema.Struct({ volume: Name, environment: Name, to: Name, wait: Seconds }),
    gate: "approval",
    idempotency: "volume_mirror",
    surfaces: {
      cli: {
        route: VOLUME_RUNS,
        body: VOLUME_RUN_BODIES.mirror,
        options: { volume: { via: ":id" }, environment: { via: "environment" }, to: { via: "to" }, wait: CLI_FOLLOWS },
      },
      agent: {
        tool: "volume_mirror",
        options: { volume: { via: "volume" }, environment: AGENT_ENVIRONMENT, to: { via: "server" }, wait: { via: "wait_seconds" } },
      },
      ui: STAGED,
    },
  }),
  "volume.sync": operation({
    options: Schema.Struct({ volume: Name, environment: Name, full: Schema.Boolean, wait: Seconds }),
    gate: "approval",
    idempotency: "volume_mirror",
    surfaces: {
      cli: {
        route: VOLUME_RUNS,
        body: VOLUME_RUN_BODIES.sync,
        options: { volume: { via: ":id" }, environment: { via: "environment" }, full: { via: "full" }, wait: CLI_FOLLOWS },
      },
      agent: {
        tool: "volume_mirror",
        options: { volume: { via: "volume" }, environment: AGENT_ENVIRONMENT, full: { via: "full" }, wait: { via: "wait_seconds" } },
      },
      ui: {
        fn: "refreshMirrorServerFn",
        input: RefreshMirrorInput,
        options: {
          volume: { via: "volumeId" },
          environment: { via: "environment" },
          full: { omitted: "Sync now sends only what changed; a clean full send is a CLI or agent call." },
          wait: UI_FOLLOWS,
        },
      },
    },
  }),
  "volume.move": operation({
    options: Schema.Struct({ volume: Name, environment: Name, to: Name, wait: Seconds }),
    gate: "approval",
    idempotency: "volume_move",
    surfaces: {
      cli: {
        route: VOLUME_RUNS,
        body: VOLUME_RUN_BODIES.move,
        options: { volume: { via: ":id" }, environment: { via: "environment" }, to: { via: "to" }, wait: CLI_FOLLOWS },
      },
      agent: {
        tool: "volume_move",
        options: { volume: { via: "volume" }, environment: AGENT_ENVIRONMENT, to: { via: "to" }, wait: { via: "wait_seconds" } },
      },
      ui: STAGED,
    },
  }),
  "volume.release": operation({
    options: Schema.Struct({ volume: Name, environment: Name, acceptLoss: Names, wait: Seconds }),
    gate: "approval",
    idempotency: "volume_release",
    surfaces: {
      cli: {
        route: VOLUME_RUNS,
        body: VOLUME_RUN_BODIES.release,
        options: { volume: { via: ":id" }, environment: { via: "environment" }, acceptLoss: CLI_APPROVAL, wait: CLI_FOLLOWS },
      },
      agent: {
        tool: "volume_release",
        options: { volume: { via: "volume" }, environment: AGENT_ENVIRONMENT, acceptLoss: AGENT_LOSS, wait: { via: "wait_seconds" } },
      },
      ui: { absent: "The dashboard has no release control." },
    },
  }),
  "volume.remove_mirror": operation({
    options: Schema.Struct({ volume: Name, environment: Name, slot: Name, confirmName: Name, acceptLoss: Names }),
    gate: "approval",
    idempotency: "volume_mirror_remove",
    surfaces: {
      cli: {
        route: VOLUME_RUNS,
        body: VOLUME_RUN_BODIES.delete_mirror,
        options: { volume: { via: ":id" }, environment: { via: "environment" }, slot: { via: "slot" }, confirmName: { via: "confirm" }, acceptLoss: CLI_APPROVAL },
      },
      agent: {
        tool: "volume_mirror_remove",
        options: {
          volume: { via: "volume" },
          environment: AGENT_ENVIRONMENT,
          slot: { omitted: "The agent removes the Volume's mirror and never names an old copy." },
          confirmName: { omitted: "accept_volume_loss binds the loss instead of a typed name." },
          acceptLoss: AGENT_LOSS,
        },
      },
      ui: {
        fn: "deleteCopyServerFn",
        input: DeleteCopyInput,
        ungated: "The human confirms in the page's own dialog.",
        options: {
          volume: { via: "volumeId" },
          environment: { via: "environment" },
          slot: { via: "server" },
          confirmName: { omitted: "The page's own dialog confirms." },
          acceptLoss: { omitted: "The page's own dialog confirms." },
        },
      },
    },
  }),
  "volume.restore": operation({
    options: Schema.Struct({ volume: Name, environment: Name, from: Name }),
    gate: "approval",
    idempotency: "volume_restore",
    surfaces: {
      cli: { route: VOLUME_RUNS, body: VOLUME_RUN_BODIES.restore, options: { volume: { via: ":id" }, environment: { via: "environment" }, from: { via: "from" } } },
      agent: { absent: "No agent tool restores from a Server's copy." },
      ui: STAGED,
    },
  }),
  "volume.seed": operation({
    options: Schema.Struct({ volume: Name, environment: Name, guid: Name, name: Name, at: Schema.Number }),
    gate: "approval",
    idempotency: "volume_seed",
    surfaces: {
      cli: { absent: "The CLI's restore copies from a Server (volume.restore); restoring a snapshot is the dashboard's." },
      agent: { absent: "No agent tool restores a snapshot." },
      ui: {
        fn: "stageRestoreServerFn",
        input: StageRestoreInput,
        options: { volume: { via: "volumeId" }, environment: { via: "environment" }, guid: { via: "guid" }, name: { via: "name" }, at: { via: "at" } },
      },
    },
  }),
  "servers.forget_all": operation({
    options: Schema.Struct({ organization: Name }),
    gate: "none",
    idempotency: { none: "It retries every Server and forgets only when none answers, so a second call finds nothing." },
    surfaces: {
      cli: { route: "POST forget-servers", body: ForgetServers, options: { organization: { via: "organization" } } },
      agent: { absent: "Forgetting every Server stays with the human." },
      ui: {
        fn: "forgetServersServerFn",
        input: ForgetServersInput,
        options: { organization: { omitted: "The page's own dialog confirms; organizationSlug only addresses the call." } },
      },
    },
  }),
} as const;

export type OperationName = keyof typeof OPERATIONS;

/** The idempotency tool of each Volume Run kind. */
export const VOLUME_TOOLS = {
  mirror: OPERATIONS["volume.mirror"].idempotency,
  sync: OPERATIONS["volume.sync"].idempotency,
  move: OPERATIONS["volume.move"].idempotency,
  release: OPERATIONS["volume.release"].idempotency,
  delete_mirror: OPERATIONS["volume.remove_mirror"].idempotency,
  restore: OPERATIONS["volume.restore"].idempotency,
  seed: OPERATIONS["volume.seed"].idempotency,
} as const satisfies Record<VolumeRunKind, OperationTool>;
