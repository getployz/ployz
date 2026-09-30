import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { listStrayNamespaces, loadNamespaceDataLoss, removeNamespace } from "./namespace-cleanup.server";

const Namespace = Schema.Struct({ organizationSlug: Schema.NonEmptyString, namespace: Schema.NonEmptyString });

export const listStrayNamespacesServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString, namespaces: Schema.Array(Schema.NonEmptyString) })))
  .handler(({ context, data }) => runActor(context, listStrayNamespaces(context.actor, data)));

export const loadNamespaceDataLossServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Namespace))
  .handler(({ context, data }) => runActor(context, Effect.scoped(loadNamespaceDataLoss(context.actor, data))));

export const removeNamespaceServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...Namespace.fields, confirmDataLoss: Schema.Array(dataLossIdentitySchema) })))
  .handler(({ context, data }) => runActor(context, removeNamespace(context.actor, data)));
