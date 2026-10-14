import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Effect, Option, Schema } from "effect";
import { getClusterDomainCollection, getOrganizationEnrollmentCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { prefetchStoreViews } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { isSingleSettingsVariant } from "#/components/dashboard-navigation-model";
import { buttonVariants } from "#/components/ui/button-variants";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { VariantSwitcher } from "#/routes/_protected/cloud/account/-components/account-sections";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { BUILD_ORDER_QUERY, BuildsSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/builds-settings";
import { PendingEnrollmentResetSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection";
import { ClusterDomainSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection";
import { checkClusterDomainNowServerFn } from "#/modules/cluster-domain/cluster-domain.functions";
import { projectsQuery } from "#/modules/config-store/store-view.queries";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";
import { SupportAccessSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/support-access-section";
import { StoreOrganizationDanger } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/store-organization-danger";

const settingsSectionSchema = Schema.Literals(["general", "builds"]);

// Not `tab`: search keys share one type across routes, and the service panel's `tab` means something else.
const settingsSearchSchema = Schema.Struct({
  section: Schema.optional(settingsSectionSchema.pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("general" as const))),
  )),
});

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/settings",
)({
  validateSearch: Schema.toStandardSchemaV1(settingsSearchSchema),
  loader: ({ params, context }) => prefetchStoreViews(context, params.organizationSlug, projectsQuery(), BUILD_ORDER_QUERY),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { section = "general", variant } = Route.useSearch();
  // Only a Cloud with Ployz support offers to let it in.
  const state = useQuery(organizationStateQueryOptions(organizationSlug)).data;
  const supportAvailable = state?.supportAvailable === true;

  // Prototype F and G: every section on one page, Danger last.
  if (isSingleSettingsVariant(variant)) {
    return (
      <DashboardPage width="content">
        <div className="flex flex-col gap-6">
          <EnrollmentSection organizationSlug={organizationSlug} />
          <ClusterDomainSettings organizationSlug={organizationSlug} />
          {/* Builds is a group of sections, so it draws the hairline its first one leaves off. */}
          <div className="border-t pt-6"><BuildsSettings organizationSlug={organizationSlug} /></div>
          {supportAvailable ? <SupportAccessSection organizationSlug={organizationSlug} /> : null}
          {state?.billingEnabled ? (
            <SettingsSection id="billing" title="Billing" description="Free on your own servers."
              action={<Link to="/cloud/$organizationSlug/~/billing" params={{ organizationSlug }} className={buttonVariants({ variant: "outline" })}>Open billing</Link>}>
              {null}
            </SettingsSection>
          ) : null}
          <StoreOrganizationDanger organizationSlug={organizationSlug} />
        </div>
        {variant ? <VariantSwitcher current={variant} /> : null}
      </DashboardPage>
    );
  }

  return (
    <DashboardPage width="content">
      {/* The top bar names the page; the rail, or on phones the strip under the top bar, picks the section. */}
      {section === "builds" ? <BuildsSettings organizationSlug={organizationSlug} /> : (
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
