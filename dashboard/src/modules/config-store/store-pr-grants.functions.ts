import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { listMissingStorePrGrants } from "./store-pr-grants.server";

export const listMissingStorePrGrantsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString, projectSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, listMissingStorePrGrants(context.actor, data)));
