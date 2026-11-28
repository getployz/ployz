import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { approvePasskeyJoin, pendingPasskeyJoins } from "#/modules/machines/add-server/passkey-join.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

const OrganizationSlug = Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty());

const PendingJoinsInput = Schema.Struct({ organizationSlug: OrganizationSlug });
/** The passkey-answered step as JSON text; the Server parses and checks all of it before it takes the approval. */
const ApproveJoinInput = Schema.Struct({ organizationSlug: OrganizationSlug, approval: Schema.String.check(Schema.isMaxLength(64_000)) });

export const pendingPasskeyJoinsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(PendingJoinsInput))
  .handler(({ context, data }) => runActor(context, pendingPasskeyJoins(context.actor, data)));

export const approvePasskeyJoinServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(ApproveJoinInput))
  .handler(({ context, data }) => runActor(context, approvePasskeyJoin(context.actor, data)));
