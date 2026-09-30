import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { OrganizationSlug, SyncOrganizationSlug } from "./organization-state";
import { getOrganizationState, openOrCreateOrganization, syncOrganizationSlug } from "./organization-state.server";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

const OrganizationStateInput = Schema.Struct({
  organizationSlug: Schema.optionalKey(OrganizationSlug),
});

export const getOrganizationStateServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(OrganizationStateInput))
  .handler(({ context, data }) => runActor(context, getOrganizationState(context.actor, data.organizationSlug)));

export const syncOrganizationSlugServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(SyncOrganizationSlug))
  .handler(({ context, data }) => runActor(context, syncOrganizationSlug(context.actor, data)));

/** Opens the actor's first Organization, or creates one: for a session that acts in none. */
export const openOrCreateOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .handler(({ context }) => runActor(context, openOrCreateOrganization(context.actor)));
