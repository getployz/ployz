import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { CreateGoalInput, DeleteGoalInput, ReadAnalyticsViewInput, TurnOnAnalyticsInput } from "./analytics-model";
import { createGoal, deleteGoal, readAnalyticsView, turnOnAnalytics } from "./analytics.server";

export const readAnalyticsViewServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadAnalyticsViewInput))
  .handler(({ context, data }) => runActor(context, readAnalyticsView(context.actor, data)));

export const turnOnAnalyticsServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(TurnOnAnalyticsInput))
  .handler(({ context, data }) => runActor(context, turnOnAnalytics(context.actor, data)));

export const createGoalServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(CreateGoalInput))
  .handler(({ context, data }) => runActor(context, createGoal(context.actor, data)));

export const deleteGoalServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(DeleteGoalInput))
  .handler(({ context, data }) => runActor(context, deleteGoal(context.actor, data)));
