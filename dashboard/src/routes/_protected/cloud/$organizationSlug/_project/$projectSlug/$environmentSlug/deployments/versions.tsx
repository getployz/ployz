import { createFileRoute } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { EnvironmentHistory } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentHistory";
import { DeploymentsPageError, DeploymentsPagePending } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/deployments/DeploymentsPageStates";

/** The Environment's Saved versions. `revision` picks one out. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments/versions")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    revision: Schema.optional(Schema.Number.pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
  })),
  pendingComponent: DeploymentsPagePending,
  errorComponent: () => <DeploymentsPageError noun="Versions" />,
  component: RouteComponent,
});

function RouteComponent() {
  return <EnvironmentHistory revision={Route.useSearch().revision} />;
}
