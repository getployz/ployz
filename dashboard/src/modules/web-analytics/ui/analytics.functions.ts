import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { CreateGoalInput, DeleteGoalInput, ReadAnalyticsViewInput, ReadRequestViewInput, TurnOnAnalyticsInput } from "./analytics-model";
import { createGoal, deleteGoal, readAnalyticsView, readApiView, readErrorsView, turnOnAnalytics } from "./analytics.server";

export const readAnalyticsViewServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadAnalyticsViewInput))
  .handler(({ context, data }) => runActor(context, readAnalyticsView(context.actor, data)));

export const readApiViewServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadRequestViewInput))
  .handler(({ context, data }) => runActor(context, readApiView(context.actor, data)));

export const readErrorsViewServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadRequestViewInput))
  .handler(({ context, data }) => runActor(context, readErrorsView(context.actor, data)));

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
