import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect } from "effect";
import { RenameClusterInput } from "#/modules/cluster-name/cluster-name";
import { readClusterName } from "#/modules/cluster-name/cluster-name.server";
import { runCommand } from "#/modules/commands/commands.server";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { Validation } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const renameClusterServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RenameClusterInput))
  .handler(({ context, data: { organizationSlug, name } }) => runActor(context, Effect.gen(function* () {
    const caller = yield* dashboardCaller(getRequest().headers, organizationSlug);
    const ran = yield* runCommand(caller, "cluster_rename", { name }, { approvalId: null, key: null });
    if (!ran.ok) return yield* new Validation({ message: ran.refusal.message, userFacing: true });
    const row = yield* readClusterName(caller.organization.id);
    return row ?? (yield* Effect.die("a rename left no row"));
  })));
