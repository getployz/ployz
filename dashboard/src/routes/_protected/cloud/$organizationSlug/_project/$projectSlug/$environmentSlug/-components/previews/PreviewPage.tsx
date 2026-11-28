import { useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { ChevronDownIcon } from "lucide-react";
import { toast } from "sonner";
import type { PreviewRow, PreviewSettings, PreviewsView } from "@ployz/sdk";
import { DashboardPage } from "#/components/dashboard-page";
import { RelativeTime } from "#/components/relative-time";
import { SettingsDanger, SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Button } from "#/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Spinner } from "#/components/ui/spinner";
import { PREVIEW_SETTINGS_ROUTE_TO, PREVIEWS_ROUTE_TO, type PreviewRouteParams } from "#/lib/environment-routes";
import { listNames } from "#/lib/plural";
import { useOrganizationName } from "#/modules/organization/organization-state.queries";
import { ruleBody } from "#/modules/previews/previews.contract";
import { usePreviews } from "#/modules/previews/previews.queries";
import { usePreviewWrites, writeFailure } from "#/modules/previews/preview-writes";
import {
  addressHref, authorLine, CloseOneList, deployable, isUp, outcomeLine, PreviewsRefused, PreviewStatus, RaiseLimit, shortCommit,
  useClosePreview, useServerName,
} from "./preview-parts";

/**
 * One pull request's preview, and the page the GitHub comment's link opens. Live, it says what the preview is and
 * closes it. Not deployed, it says what one Deploy would run and deploys it; at the limit it lists the live previews to
 * close one; with previews off it sets them up.
 */
export function PreviewPage({ params, repository, number }: { params: PreviewRouteParams; repository: string; number: number }) {
  const result = usePreviews(params.organizationSlug, { project: params.projectSlug, repository, number });
  if (!result.ok) return <DashboardPage width="content" title={`#${number}`}><PreviewsRefused message={result.refusal.message} /></DashboardPage>;
  const view = result.value;
  const row = view.pull_requests.find((candidate) => candidate.number === number);
  if (row === undefined) {
    return (
      <DashboardPage width="content" title={`#${number}`}>
        <Empty variant="placeholder">
          <EmptyHeader>
            <EmptyTitle>No pull request #{number} on {view.repository}</EmptyTitle>
            <EmptyDescription>It may have moved to another repository.</EmptyDescription>
          </EmptyHeader>
          <EmptyContent>
            <Button variant="outline" render={<Link to={PREVIEWS_ROUTE_TO} params={params} />}>Pull requests</Button>
          </EmptyContent>
        </Empty>
      </DashboardPage>
    );
  }
  return <Preview view={view} row={row} params={params} />;
}

function Preview({ view, row, params }: { view: PreviewsView; row: PreviewRow; params: PreviewRouteParams }) {
  const organizationName = useOrganizationName(params.organizationSlug);
  const outcome = row.preview;
  const settings = view.settings;
  const live = outcome.state === "live" && outcome.address ? outcome.address : null;
  return (
    <DashboardPage width="content" title={`#${row.number} ${row.title}`}
      actions={live ? <Button render={<a href={addressHref(live)} target="_blank" rel="noreferrer" />}>Open</Button> : undefined}>
      <div className="flex flex-col gap-2">
        <p className="text-sm text-muted-foreground">
          {authorLine(row)}{row.head ? <>, latest commit <span className="font-mono">{shortCommit(row.head)}</span></> : null}, <RelativeTime date={new Date(row.updated)} />
        </p>
        <PreviewStatus line={outcomeLine(row, view, organizationName)} />
      </div>
      {settings === null ? <Off params={params} repository={view.repository} />
        : isUp(outcome) ? <Up row={row} settings={settings} params={params} />
        : outcome.state === "not_deployed" && outcome.why?.kind === "full" && row.open && view.live >= view.limit
          ? <Full view={view} row={row} params={params} />
          : <Confirm view={view} row={row} settings={settings} params={params} />}
    </DashboardPage>
  );
}

function Off({ params, repository }: { params: PreviewRouteParams; repository: string }) {
  return (
    <Empty variant="placeholder">
      <EmptyHeader>
        <EmptyTitle>Previews are off for {repository}</EmptyTitle>
        <EmptyDescription>Set them up to deploy pull requests as a copy of an environment.</EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button render={<Link to={PREVIEW_SETTINGS_ROUTE_TO} params={params} search={{ edit: true }} />}>Set up previews</Button>
      </EmptyContent>
    </Empty>
  );
}

/** The Services a preview runs: those the rule builds from the pull request, then those it runs by image. */
function servicesOf(settings: PreviewSettings) {
  const rule = ruleBody(settings.rule);
  const pinned = new Set(rule?.preview?.pinned?.map((service) => service.name));
  const built = rule?.services.map((service) => service.name).filter((name) => !pinned.has(name)) ?? [];
  return { built, pinned: [...pinned] };
}

/** What one Deploy runs, and the Deploy. */
function Confirm({ view, row, settings, params }: { view: PreviewsView; row: PreviewRow; settings: PreviewSettings; params: PreviewRouteParams }) {
  const writes = usePreviewWrites(params.organizationSlug);
  const serverName = useServerName(params.organizationSlug);
  const [deploying, setDeploying] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const { built, pinned } = servicesOf(settings);
  const head = row.head;
  const ready = deployable(row, view) && head !== null;
  const deploy = async () => {
    if (head === null) return;
    setDeploying(true);
    setRefused(null);
    try {
      await writes.deploy({ repository_id: view.repository_id, number: row.number, head });
      toast.success(`Deploying #${row.number}`);
    } catch (error) {
      setRefused(writeFailure(error));
    } finally {
      setDeploying(false);
    }
  };
  return (
    <SettingsSection id="deploys" title="What deploys">
      <Fact label="Pull request">#{row.number} {row.title}, by {authorLine(row)}</Fact>
      <Fact label="Commit">{head ? <span className="font-mono">{shortCommit(head)}</span> : "None yet"}</Fact>
      <Fact label="Copy of">{settings.start_from}, with its secrets</Fact>
      <Fact label="Services">
        {[built.length > 0 ? `${listNames(built)} from the pull request` : null, pinned.length > 0 ? `${listNames(pinned)} as in ${settings.start_from}` : null]
          .filter(Boolean).join("; ") || "As in " + settings.start_from}
      </Fact>
      <Fact label="Server">{settings.servers.length === 1 ? serverName(settings.servers[0] ?? "") : `One of ${listNames(settings.servers.map(serverName))}`}</Fact>
      <SettingsRow label="Its databases start empty" hint={refused ?? undefined} commit="none" control={
        <Button disabled={!ready || deploying} onClick={() => void deploy()}>
          {deploying ? <Spinner data-icon="inline-start" /> : null}
          Deploy
        </Button>
      } />
    </SettingsSection>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return <SettingsRow label={label} commit="none" control={<span className="text-sm">{children}</span>} />;
}

/** At the limit: close a live preview, or raise the limit, to deploy this one. */
function Full({ view, row, params }: { view: PreviewsView; row: PreviewRow; params: PreviewRouteParams }) {
  return (
    <section aria-labelledby="close-one" className="flex flex-col gap-3">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="flex flex-col gap-1">
          <h2 id="close-one" className="text-base font-semibold">{view.live} of {view.limit} previews are live</h2>
          <p className="text-sm text-muted-foreground">Close one to deploy #{row.number}, or raise the limit.</p>
        </div>
        <RaiseLimit params={params} />
      </div>
      <CloseOneList view={view} params={params} project={params.projectSlug} organizationSlug={params.organizationSlug} />
    </section>
  );
}

/** A live or deploying preview: what it is, and closing it. */
function Up({ row, settings, params }: { row: PreviewRow; settings: PreviewSettings; params: PreviewRouteParams }) {
  const serverName = useServerName(params.organizationSlug);
  const close = useClosePreview(params.organizationSlug, params.projectSlug);
  const outcome = row.preview;
  if (!isUp(outcome)) return null;
  const address = outcome.state === "live" ? outcome.address : undefined;
  const server = outcome.state === "live" ? outcome.server : undefined;
  const newer = outcome.state === "live" ? outcome.newer : undefined;
  return (
    <>
      <SettingsSection id="what" title="What it is">
        <Fact label="Copy of">{settings.start_from}, with its secrets and its own databases</Fact>
        <Fact label="Address">{address ? <span className="font-mono">{address}</span> : "Given when it deploys"}</Fact>
        <Fact label="Commit">
          <span className="font-mono">{shortCommit(outcome.head)}</span>
          {newer ? <>, <span className="font-mono">{shortCommit(newer)}</span> not deployed yet</> : null}
        </Fact>
        <Fact label="Server">{server ? serverName(server) : "Chosen when it deploys"}</Fact>
        <Fact label="Last push"><RelativeTime date={new Date(row.updated)} /></Fact>
        <Collapsible className="border-t">
          <CollapsibleTrigger render={<Button variant="ghost" size="sm" className="m-2" />}>
            Details <ChevronDownIcon data-icon="inline-end" />
          </CollapsibleTrigger>
          <CollapsibleContent>
            <Fact label="Allowed by">
              Preview settings, version {settings.generation}, signed <RelativeTime date={new Date(settings.issued_at)} />
            </Fact>
            <Fact label="Environment"><span className="font-mono">{outcome.environment}</span></Fact>
          </CollapsibleContent>
        </Collapsible>
      </SettingsSection>
      <SettingsSection id="close" title="Close preview">
        <SettingsDanger label="Close preview" hint="Deletes the copy and its databases. It also closes when the pull request closes."
          action={<Button variant="destructive" onClick={() => close.ask(row)}>Close preview</Button>} />
      </SettingsSection>
      {close.dialog}
    </>
  );
}
