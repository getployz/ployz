import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { StrictOrganizationSlug } from "#/lib/schema";
import { cancelServerEnrollment, previewServerEnrollment, readServerEnrollmentStatus, startServerEnrollment } from "#/modules/machines/waiting/enroll.server";
import { EnrollInput } from "#/modules/machines/waiting/server-enrollment";
import { OpenProbeSelectionInput, SELECTION_MAX } from "#/modules/machines/waiting/probe";
import { openProbeSelection } from "#/modules/machines/waiting/probe.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import { listWaitingServers, showWaitingServer } from "#/modules/machines/waiting/waiting-servers.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

const ListWaitingServersInput = Schema.Struct({ organizationSlug: StrictOrganizationSlug });
const ShowWaitingServerInput = Schema.Struct({ organizationSlug: StrictOrganizationSlug, id: Schema.String.check(Schema.isUUID()) });
const RejectWaitingServersInput = Schema.Struct({
  organizationSlug: StrictOrganizationSlug,
  ids: Schema.Array(Schema.String.check(Schema.isUUID())).check(Schema.isMinLength(1), Schema.isMaxLength(SELECTION_MAX)),
});

export const openProbeSelectionServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(OpenProbeSelectionInput))
  .handler(({ context, data }) => runActor(context, openProbeSelection(context.actor, data)));

export const listWaitingServersServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(ListWaitingServersInput))
  .handler(({ context, data }) => runActor(context, listWaitingServers(context.actor, data)));

export const showWaitingServerServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(ShowWaitingServerInput))
  .handler(({ context, data }) => runActor(context, showWaitingServer(context.actor, data)));

export const rejectWaitingServersServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(RejectWaitingServersInput))
  .handler(({ context, data }) => runActor(context, rejectWaitingServers(context.actor, data)));

const DashboardEnrollInput = Schema.Struct({
  ...EnrollInput.fields,
  organizationSlug: StrictOrganizationSlug,
  ids: Schema.Array(Schema.String.check(Schema.isUUID())).check(Schema.isMinLength(1), Schema.isMaxLength(SELECTION_MAX)),
});

export const planServerEnrollmentServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(DashboardEnrollInput))
  .handler(({ context, data }) => runActor(context, previewServerEnrollment(context.actor, data)));

export const startServerEnrollmentServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(DashboardEnrollInput))
  .handler(({ context, data }) => runActor(context, startServerEnrollment(context.actor, data)));

export const serverEnrollmentStatusServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(ListWaitingServersInput))
  .handler(({ context, data }) => runActor(context, readServerEnrollmentStatus(context.actor, data)));

export const cancelServerEnrollmentServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(ShowWaitingServerInput))
  .handler(({ context, data }) => runActor(context, cancelServerEnrollment(context.actor, data)));
