import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";
import { listMissingStorePrGrants } from "./store-pr-grants.server";

export const listMissingStorePrGrantsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString, projectSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, listMissingStorePrGrants(context.actor, data)));
