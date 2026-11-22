import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";
import { RemoveNamespaceInput } from "./namespace-cleanup";
import { listStrayNamespaces, loadNamespaceDataLoss, removeNamespace } from "./namespace-cleanup.server";

const Namespace = Schema.Struct({ organizationSlug: Schema.NonEmptyString, namespace: Schema.NonEmptyString });

export const listStrayNamespacesServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString, namespaces: Schema.Array(Schema.NonEmptyString) })))
  .handler(({ context, data }) => runActor(context, listStrayNamespaces(context.actor, data)));

export const loadNamespaceDataLossServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Namespace))
  .handler(({ context, data }) => runActor(context, Effect.scoped(loadNamespaceDataLoss(context.actor, data))));

export const removeNamespaceServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(RemoveNamespaceInput))
  .handler(({ context, data }) => runActor(context, removeNamespace(context.actor, data)));
