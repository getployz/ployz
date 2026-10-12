import { createServerFn } from "@tanstack/react-start";
import { hasMarketingSite } from "#/server/marketing-proxy.server";
import { runAppEffect } from "#/server/run.server";
import { publicErrorMiddleware } from "#/server/tanstack";

/** Public: the signed-out /auth page asks it. */
export const hasMarketingSiteServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware])
  .handler(() => runAppEffect(hasMarketingSite));
