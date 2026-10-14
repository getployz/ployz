import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { useQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Effect, Option, Schema } from "effect";
import { getClusterDomainCollection, getOrganizationEnrollmentCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { prefetchRemoteWithStoreViews } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { CONNECT_NOTICES } from "#/modules/github/github-connect";
import { githubOfferQueryOptions, organizationGithubQueryOptions } from "#/modules/github/github-connect.queries";
import { GithubSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/github-settings";
import { BUILD_ORDER_QUERY, BuildsSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/builds-settings";
import { PendingEnrollmentResetSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection";
import { ClusterDomainSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection";
import { checkClusterDomainNowServerFn } from "#/modules/cluster-domain/cluster-domain.functions";
import { projectsQuery } from "#/modules/config-store/store-view.queries";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";
import { SupportAccessSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/support-access-section";
import { StoreOrganizationDanger } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/store-organization-danger";

const settingsSectionSchema = Schema.Literals(["general", "github", "builds"]);

// Not `tab`: search keys share one type across routes, and the service panel's `tab` means something else.
const settingsSearchSchema = Schema.Struct({
  section: Schema.optional(settingsSectionSchema.pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("general" as const))),
  )),
  connect: Schema.optional(Schema.Literals(CONNECT_NOTICES).pipe(Schema.catchDecoding(() => Effect.succeedNone))),
  offer: Schema.optional(Schema.String.check(Schema.isMaxLength(8192)).pipe(Schema.catchDecoding(() => Effect.succeedNone))),
});

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/settings",
)({
  validateSearch: Schema.toStandardSchemaV1(settingsSearchSchema),
  loaderDeps: ({ search }) => ({ section: search.section, offer: search.offer }),
  loader: ({ params, context, deps }) => prefetchRemoteWithStoreViews(context, params.organizationSlug, [projectsQuery(), BUILD_ORDER_QUERY],
    ...deps.section === "github" ? [organizationGithubQueryOptions(params.organizationSlug)] : [],
    ...deps.section === "github" && deps.offer ? [githubOfferQueryOptions(params.organizationSlug, deps.offer)] : []),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { section = "general", connect, offer } = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const supportAvailable = useQuery(organizationStateQueryOptions(organizationSlug)).data?.supportAvailable === true;

  return (
    <DashboardPage width="content">
      {/* The top bar names the page; the rail, or on phones the strip under the top bar, picks the section. */}
      {section === "builds" ? <BuildsSettings organizationSlug={organizationSlug} /> : section === "github" ? (
        <GithubSettings
          organizationSlug={organizationSlug}
          notice={connect}
          offer={offer}
          clearConnectSearch={() => void navigate({ search: { section }, replace: true })}
        />
      ) : (
        <div className="flex flex-col gap-6">
          <EnrollmentSection organizationSlug={organizationSlug} />
          <ClusterDomainSettings organizationSlug={organizationSlug} />
          {supportAvailable ? <SupportAccessSection organizationSlug={organizationSlug} /> : null}
          <StoreOrganizationDanger organizationSlug={organizationSlug} />
        </div>
      )}
    </DashboardPage>
  );
}

function EnrollmentSection({ organizationSlug }: { organizationSlug: string }) {
  const enrollment = getOrganizationEnrollmentCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(enrollment);
  const status = organizationEnrollmentStatus(rows[0]);
  const resetPendingEnrollment = useServerFn(resetPendingOrganizationEnrollmentServerFn);
  return (
    <PendingEnrollmentResetSection
      status={status}
      onReset={({ confirmedFounderStoppedOrErased }) =>
        resetPendingEnrollment({
          data: { organizationSlug, confirmedFounderStoppedOrErased },
        }).then(() => undefined)
      }
      onCompleted={() => {
        void reconcileCollection(enrollment);
      }}
    />
  );
}

function ClusterDomainSettings({ organizationSlug }: { organizationSlug: string }) {
  const { data: rows } = useLiveSuspenseQuery(getClusterDomainCollection(organizationSlug, useCollectionScope()));
  const checkNow = useServerFn(checkClusterDomainNowServerFn);
  return (
    <ClusterDomainSection
      organizationSlug={organizationSlug}
      domain={rows[0] ?? null}
      onCheck={() => checkNow({ data: { organizationSlug } })}
    />
  );
}
