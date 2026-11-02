import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { disconnectAgent, listAccountSessions, listConnectedAgents, listLinkedAccounts, revokeAccountSession } from "#/modules/account/account.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const listAccountSessionsServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .handler(({ context }) => runActor(context, listAccountSessions(context.actor)));

export const listLinkedAccountsServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .handler(({ context }) => runActor(context, listLinkedAccounts(context.actor)));

export const revokeAccountSessionServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(Schema.Struct({ sessionId: Schema.String })))
  .handler(({ context, data }) => runActor(context, revokeAccountSession(context.actor, data.sessionId)));

export const listConnectedAgentsServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .handler(({ context }) => runActor(context, listConnectedAgents(context.actor)));

export const disconnectAgentServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(Schema.Struct({ consentId: Uuid })))
  .handler(({ context, data }) => runActor(context, disconnectAgent(context.actor, data.consentId)));
