import { createServerFn } from "@tanstack/react-start";
import { RequestServerPolicyChangeInput } from "#/modules/machines/server-policy";
import { requestServerPolicyChange } from "#/modules/machines/server-policy.server";
import {
  authenticated,
  runActor,
  strictValidator,
} from "#/server/tanstack";

export const requestServerPolicyChangeServerFn = createServerFn({
  method: "POST",
})
  .middleware(authenticated)
  .validator(strictValidator(RequestServerPolicyChangeInput))
  .handler(({ context, data }) =>
    runActor(context, requestServerPolicyChange(context.actor, data)),
  );
