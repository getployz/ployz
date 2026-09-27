import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { createBranch, setBranchSetupDefaults } from "./branch-operations.server";
import { mergeBranch } from "./branch-merge.server";
import { CreateBranch, MergeBranch, SetBranchSetupDefaults } from "./branch-schemas";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const createBranchServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(CreateBranch))
  .handler(({ context, data }) => runActor(context, createBranch(context.actor, data)));

export const setBranchSetupDefaultsServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(SetBranchSetupDefaults))
  .handler(({ context, data }) => runActor(context, setBranchSetupDefaults(context.actor, data)));

export const mergeBranchServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(MergeBranch))
  .handler(({ context, data }) => runActor(context, mergeBranch(context.actor, data)));
