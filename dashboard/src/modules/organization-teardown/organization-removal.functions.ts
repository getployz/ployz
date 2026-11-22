import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { removeOrganizationAsMember } from "./organization-removal.server";

/** Delete organization over the Config Store: refused (as data) while a Project remains; kept while a Server hasn't confirmed. */
export const removeOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String })))
  .handler(({ context, data }) => runActor(context, removeOrganizationAsMember(context.actor, data.organizationSlug)));
