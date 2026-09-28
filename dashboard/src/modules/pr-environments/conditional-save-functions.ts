import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { approveConditionalSave, giveConditionalSaveValue, withdrawConditionalSave } from "./conditional-save.server";
import { ApproveConditionalSave, GiveConditionalSaveValue, WithdrawConditionalSave } from "./conditional-save";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const approveConditionalSaveServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(ApproveConditionalSave))
  .handler(({ context, data }) => runActor(context, approveConditionalSave(context.actor, data)));

export const withdrawConditionalSaveServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(WithdrawConditionalSave))
  .handler(({ context, data }) => runActor(context, withdrawConditionalSave(context.actor, data)));

export const giveConditionalSaveValueServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(GiveConditionalSaveValue))
  .handler(({ context, data }) => runActor(context, giveConditionalSaveValue(context.actor, data)));
