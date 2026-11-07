import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { ClusterName } from "#/modules/cluster-name/cluster-name";
import { renameAsked, renameCluster } from "#/modules/cluster-name/cluster-name.server";
import { defineCommand, refused } from "#/modules/commands/command.server";
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
    asked: (caller, input) => renameAsked(caller.organization.id, input.name),
  },
  agents: true,
  typed: null,
  positional: ["name"],
  input: Schema.Struct({ name: ClusterName.annotate({ description: "The new name: one DNS label, such as acme" }) }),
  output: Schema.Struct({ name: Schema.String }),
  render: "message",
  run: (caller, input) => caller.support !== null
    ? Effect.succeed(refused("forbidden", "Only an owner or admin of this Organization can do this."))
    : renameCluster(caller.organization.id, input.name).pipe(
      Effect.map(({ name }) => ({ ok: true, output: { name }, message: `Named the cluster ${name}.` }) as const),
    ),
});
