import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { ClusterName } from "#/modules/cluster-name/cluster-name";
import { readClusterName, renameCluster } from "#/modules/cluster-name/cluster-name.server";
import { defineCommand } from "#/modules/commands/command.server";
import { OPERATIONS } from "#/modules/operations/operations";

const rename = OPERATIONS["cluster.rename"];

export const clusterRename = defineCommand({
  name: "cluster_rename",
  requires: rename.requires,
  path: ["cluster", "rename"],
  about: "Rename the cluster, the <name> in <service>.<namespace>.<name>.ployz.internal",
  examples: ["ployz cluster rename acme"],
  kind: "write",
  removes: false,
  gate: {
    gate: "ask",
    asked: (caller, input) => readClusterName(caller.organization.id).pipe(Effect.map((row) => ({
      subject: `cluster:${caller.organization.id}`,
      verb: "set_cluster_name",
      name: input.name,
      preview: { from: row?.name ?? null, to: input.name },
      effects: [],
      alwaysAsks: true,
    }))),
  },
  agents: true,
  typed: null,
  positional: ["name"],
  input: Schema.Struct({ name: ClusterName.annotate({ description: "The new name: one DNS label, such as acme" }) }),
  output: Schema.Struct({ name: Schema.String }),
  render: "message",
  run: (caller, input) => renameCluster(caller.organization.id, input.name).pipe(
    Effect.map(({ name }) => ({ ok: true, output: { name }, message: `Named the cluster ${name}.` }) as const),
  ),
});
