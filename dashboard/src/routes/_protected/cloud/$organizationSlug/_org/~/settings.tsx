import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { useQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import type { ReactNode } from "react";
import { Effect, Option, Schema } from "effect";
import { getOrganizationEnrollmentCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { prefetchRemote, prefetchRemoteWithStoreViews } from "#/collections/route-data";
import { DashboardPage } from "#/components/dashboard-page";
import { ORGANIZATION_SETTINGS_SECTIONS, type OrganizationSettingsSection } from "#/components/shell/dashboard-navigation-model";
import { organizationEnrollmentStatus } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { CONNECT_NOTICES } from "#/modules/github/github-connect";
import { clusterIngressQueryOptions } from "#/modules/ingress/doors.queries";
import { githubWebQueryOptions } from "#/modules/github/github.queries";
import { githubOfferQueryOptions, organizationGithubQueryOptions } from "#/modules/github/github-connect.queries";
import { GithubSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/github-settings";
import { BUILD_ORDER_QUERY, BuildsSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/builds-settings";
import { PendingEnrollmentResetSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection";
import { NetworkSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/NetworkSection";
import { projectsQuery } from "#/modules/config-store/store-view.queries";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import { organizationTokensQueryOptions } from "#/modules/identity/organization-token.queries";
import { TokensSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/tokens-settings";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";
import { MembersSettings } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/members-settings";
import { SupportAccessSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/support-access-section";
import { OrganizationNameSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/organization-name-section";
import { StoreOrganizationDanger } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/store-organization-danger";

const settingsSectionSchema = Schema.Literals(ORGANIZATION_SETTINGS_SECTIONS.map(({ id }) => id));

// Not `tab`: search keys share one type across routes, and the service panel's `tab` means something else.
const settingsSearchSchema = Schema.Struct({
  section: Schema.optional(settingsSectionSchema.pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("general" as const))),
  )),
  connect: Schema.optional(Schema.Literals(CONNECT_NOTICES).pipe(Schema.catchDecoding(() => Effect.succeedNone))),
  offer: Schema.optional(Schema.String.check(Schema.isMaxLength(8192)).pipe(Schema.catchDecoding(() => Effect.succeedNone))),
});

interface SectionProps {
  organizationSlug: string;
  connect: (typeof CONNECT_NOTICES)[number] | undefined;
  offer: string | undefined;
  clearConnectSearch: () => void;
}

/** What each section prefetches before it opens, and what it shows. */
const SECTION_PAGES = {
  general: { prefetch: () => Promise.resolve(), component: GeneralSection },
  network: {
    prefetch: (context, organizationSlug) => prefetchRemote(context, clusterIngressQueryOptions(organizationSlug)),
    component: ({ organizationSlug }) => <NetworkSection organizationSlug={organizationSlug} />,
  },
  members: { prefetch: () => Promise.resolve(), component: ({ organizationSlug }) => <MembersSettings organizationSlug={organizationSlug} /> },
  tokens: {
    prefetch: (context, organizationSlug) => prefetchRemote(context, organizationTokensQueryOptions(organizationSlug)),
    component: ({ organizationSlug }) => <TokensSettings organizationSlug={organizationSlug} />,
  },
  github: {
    prefetch: (context, organizationSlug, offer) => prefetchRemote(context, organizationGithubQueryOptions(organizationSlug),
      ...offer ? [githubOfferQueryOptions(organizationSlug, offer)] : []),
    component: ({ organizationSlug, connect, offer, clearConnectSearch }) => (
      <GithubSettings organizationSlug={organizationSlug} notice={connect} offer={offer} clearConnectSearch={clearConnectSearch} />
    ),
  },
  builds: {
    prefetch: (context) => prefetchRemote(context, githubWebQueryOptions()),
    component: ({ organizationSlug }) => <BuildsSettings organizationSlug={organizationSlug} />,
  },
} satisfies Record<OrganizationSettingsSection, {
  prefetch: (context: Parameters<typeof prefetchRemote>[0], organizationSlug: string, offer: string | undefined) => Promise<void>;
  component: (props: SectionProps) => ReactNode;
}>;

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/settings",
)({
  staticData: { section: "organization-settings" },
  validateSearch: Schema.toStandardSchemaV1(settingsSearchSchema),
  loaderDeps: ({ search }) => ({ section: search.section, offer: search.offer }),
  loader: ({ params, context, deps }) => {
    const section = SECTION_PAGES[deps.section ?? "general"].prefetch(context, params.organizationSlug, deps.offer);
    return prefetchRemoteWithStoreViews(context, params.organizationSlug, [projectsQuery(), BUILD_ORDER_QUERY],
      organizationMembersQueryOptions(params.organizationSlug)).then(() => section);
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { section = "general", connect, offer } = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const Section = SECTION_PAGES[section].component;

  return (
    <DashboardPage width="content" title="Organization">
      {/* The rail, or on phones the strip under the top bar, picks the section. */}
      <Section
        organizationSlug={organizationSlug}
        connect={connect}
        offer={offer}
        clearConnectSearch={() => void navigate({ search: { section }, replace: true })}
      />
    </DashboardPage>
  );
}

function GeneralSection({ organizationSlug }: SectionProps) {
  const supportAvailable = useQuery(organizationStateQueryOptions(organizationSlug)).data?.supportAvailable === true;
  return (
    <div className="flex flex-col gap-6">
      <OrganizationNameSection organizationSlug={organizationSlug} />
      <EnrollmentSection organizationSlug={organizationSlug} />
      {supportAvailable ? <SupportAccessSection organizationSlug={organizationSlug} /> : null}
      <StoreOrganizationDanger organizationSlug={organizationSlug} />
    </div>
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
