import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { checkForgetServers, forgetServers, forgetterFor } from "./forget-servers.server";

const input = strictValidator(Schema.Struct({ organizationSlug: Schema.String }));

/** Forget Servers' check as its dialog opens: every Server tried; refused (as data) when one answers. */
export const previewForgetServersServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(input)
  .handler(({ context, data }) => runActor(context, forgetterFor(context.actor, data.organizationSlug).pipe(
    Effect.flatMap(checkForgetServers),
  )));

/** Forget Servers: tries every Server again, and forgets the Cluster only when none answers. */
export const forgetServersServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(input)
  .handler(({ context, data }) => runActor(context, forgetterFor(context.actor, data.organizationSlug).pipe(
    Effect.flatMap(forgetServers),
  )));
