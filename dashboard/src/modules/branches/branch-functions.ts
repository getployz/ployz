import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { createBranch } from "./branch-operations.server";
import { CreateBranch } from "./branch-schemas";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const createBranchServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(CreateBranch))
  .handler(({ context, data }) => runActor(context, createBranch(context.actor, data)));
