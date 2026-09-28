import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { saveConditionalSave, withdrawConditionalSave } from "./conditional-save.server";
import { takePullRequestValue } from "./land.server";
import { SaveConditionalSave, TakePullRequestValue, WithdrawConditionalSave } from "./conditional-save";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const saveConditionalSaveServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(SaveConditionalSave))
  .handler(({ context, data }) => runActor(context, saveConditionalSave(context.actor, data)));

export const withdrawConditionalSaveServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(WithdrawConditionalSave))
  .handler(({ context, data }) => runActor(context, withdrawConditionalSave(context.actor, data)));

export const takePullRequestValueServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(TakePullRequestValue))
  .handler(({ context, data }) => runActor(context, takePullRequestValue(context.actor, data)));
