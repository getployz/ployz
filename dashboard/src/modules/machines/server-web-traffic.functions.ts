import { createServerFn } from "@tanstack/react-start";
import { RequestServerWebTrafficInput } from "#/modules/machines/server-web-traffic";
import { listLatestServerWebTraffic, requestServerWebTraffic } from "#/modules/machines/server-web-traffic.server";
import { authenticated, OrganizationSlugInput, runActor, strictValidator } from "#/server/tanstack";

export const listLatestServerWebTrafficServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationSlugInput))
  .handler(({ context, data }) => runActor(context, listLatestServerWebTraffic(context.actor, data)));

export const requestServerWebTrafficServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(RequestServerWebTrafficInput))
  .handler(({ context, data }) => runActor(context, requestServerWebTraffic(context.actor, data)));
