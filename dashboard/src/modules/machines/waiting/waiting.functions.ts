import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { OpenProbeSelectionInput } from "#/modules/machines/waiting/probe";
import { openProbeSelection } from "#/modules/machines/waiting/probe.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import { listWaitingServers, showWaitingServer } from "#/modules/machines/waiting/waiting-servers.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const OrganizationSlug = Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty());

const ListWaitingServersInput = Schema.Struct({ organizationSlug: OrganizationSlug });
const ShowWaitingServerInput = Schema.Struct({ organizationSlug: OrganizationSlug, id: Schema.String.check(Schema.isUUID()) });
const RejectWaitingServersInput = Schema.Struct({
  organizationSlug: OrganizationSlug,
  ids: Schema.Array(Schema.String.check(Schema.isUUID())).check(Schema.isMinLength(1), Schema.isMaxLength(1000)),
});

export const openProbeSelectionServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(OpenProbeSelectionInput))
  .handler(({ context, data }) => runActor(context, openProbeSelection(context.actor, data)));

export const listWaitingServersServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ListWaitingServersInput))
  .handler(({ context, data }) => runActor(context, listWaitingServers(context.actor, data)));

export const showWaitingServerServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ShowWaitingServerInput))
  .handler(({ context, data }) => runActor(context, showWaitingServer(context.actor, data)));

export const rejectWaitingServersServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RejectWaitingServersInput))
  .handler(({ context, data }) => runActor(context, rejectWaitingServers(context.actor, data)));
