import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";
import { CreateOrganization, OrganizationSlug, RenameOrganization, SyncOrganizationSlug } from "./organization-state";
import { createOrganization, getOrganizationState, openOrCreateOrganization, renameOrganization, syncOrganizationSlug } from "./organization-state.server";


const OrganizationStateInput = Schema.Struct({
  organizationSlug: Schema.optionalKey(OrganizationSlug),
});

export const getOrganizationStateServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationStateInput))
  .handler(({ context, data }) => runActor(context, getOrganizationState(context.actor, data.organizationSlug)));

export const syncOrganizationSlugServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(SyncOrganizationSlug))
  .handler(({ context, data }) => runActor(context, syncOrganizationSlug(context.actor, data)));

/** Opens the actor's first Organization, or creates one: for a session that acts in none. */
export const openOrCreateOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .handler(({ context }) => runActor(context, openOrCreateOrganization(context.actor)));

/** Creates an Organization the actor owns and opens it for their sessions. Returns its slug. */
export const createOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(CreateOrganization))
  .handler(({ context, data }) => runActor(context, createOrganization(context.actor, data)));

/** Renames an Organization; its slug stays. Owners and admins only. */
export const renameOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(RenameOrganization))
  .handler(({ context, data }) => runActor(context, renameOrganization(context.actor, data)));
