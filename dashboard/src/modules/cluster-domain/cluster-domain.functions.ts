import { createServerFn } from "@tanstack/react-start";
import { CheckClusterDomainInput } from "#/modules/cluster-domain/cluster-domain";
import { checkClusterDomainNow } from "#/modules/cluster-domain/cluster-domain.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

export const checkClusterDomainNowServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(CheckClusterDomainInput))
  .handler(({ context, data }) => runActor(context, checkClusterDomainNow(context.actor, data)));
