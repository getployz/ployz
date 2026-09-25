import { createServerFn } from "@tanstack/react-start";
import { CheckClusterDomainInput } from "#/modules/cluster-domain/cluster-domain";
import { checkClusterDomainNow } from "#/modules/cluster-domain/cluster-domain.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const checkClusterDomainNowServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(CheckClusterDomainInput))
  .handler(({ context, data }) => runActor(context, checkClusterDomainNow(context.actor, data)));
