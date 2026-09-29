import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { listMissingPrEnvironmentGrants, listMissingStorePrGrants } from "./grants.server";
import { setPrEnvironmentPlan } from "./plan-operations.server";
import { ListPrEnvironmentGrants, ListStorePrGrants, SetPrEnvironmentPlan } from "./plan-schemas";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const setPrEnvironmentPlanServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(SetPrEnvironmentPlan))
  .handler(({ context, data }) => runActor(context, setPrEnvironmentPlan(context.actor, data)));

export const listMissingPrEnvironmentGrantsServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(ListPrEnvironmentGrants))
  .handler(({ context, data }) => runActor(context, listMissingPrEnvironmentGrants(context.actor, data)));

export const listMissingStorePrGrantsServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(ListStorePrGrants))
  .handler(({ context, data }) => runActor(context, listMissingStorePrGrants(context.actor, data)));
