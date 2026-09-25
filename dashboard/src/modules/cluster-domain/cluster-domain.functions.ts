import { createServerFn } from "@tanstack/react-start";
import { PublishClusterDomainInput } from "#/modules/cluster-domain/cluster-domain";
import { publishClusterDomainNow } from "#/modules/cluster-domain/cluster-domain.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const publishClusterDomainNowServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(PublishClusterDomainInput))
  .handler(({ context, data }) => runActor(context, publishClusterDomainNow(context.actor, data)));
