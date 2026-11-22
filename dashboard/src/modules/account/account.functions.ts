import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { disconnectAgent, listAccountSessions, listConnectedAgents, listLinkedAccounts, revokeAccountSession } from "#/modules/account/account.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";


export const listAccountSessionsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .handler(({ context }) => runActor(context, listAccountSessions(context.actor)));

export const listLinkedAccountsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .handler(({ context }) => runActor(context, listLinkedAccounts(context.actor)));

export const revokeAccountSessionServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ sessionId: Schema.String })))
  .handler(({ context, data }) => runActor(context, revokeAccountSession(context.actor, data.sessionId)));

export const listConnectedAgentsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .handler(({ context }) => runActor(context, listConnectedAgents(context.actor)));

export const disconnectAgentServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ consentId: Uuid })))
  .handler(({ context, data }) => runActor(context, disconnectAgent(context.actor, data.consentId)));
