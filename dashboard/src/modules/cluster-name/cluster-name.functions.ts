import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import { RenameClusterInput } from "#/modules/cluster-name/cluster-name";
import { readClusterName } from "#/modules/cluster-name/cluster-name.server";
import { runCommand } from "#/modules/commands/commands.server";
import { Validation } from "#/server/public-error";
import { callerMiddleware, runActor } from "#/server/tanstack";

export const renameClusterServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(RenameClusterInput)])
  .handler(({ context, data: { name } }) => runActor(context, Effect.gen(function* () {
    const { caller } = context;
    const ran = yield* runCommand(caller, "cluster_rename", { name }, { approvalId: null, key: null });
    if (!ran.ok) return yield* new Validation({ message: ran.refusal.message, userFacing: true });
    const row = yield* readClusterName(caller.organization.id);
    return row ?? (yield* Effect.die("a rename left no row"));
  })));
