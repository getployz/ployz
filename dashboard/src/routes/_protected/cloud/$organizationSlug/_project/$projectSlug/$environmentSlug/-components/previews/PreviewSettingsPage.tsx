import { useState, type ReactNode } from "react";
import { Link, useNavigate } from "@tanstack/react-router";
import type { PreviewRuleView, PreviewSettings, PreviewsView, Signed } from "@ployz/sdk";
import { KeyRoundIcon, ServerIcon, UsersIcon } from "lucide-react";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { DashboardPage } from "#/components/dashboard-page";
import { RelativeTime } from "#/components/relative-time";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Item, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import { PREVIEWS_ROUTE_TO, type PreviewRouteParams } from "#/lib/environment-routes";
import { listNames, plural } from "#/lib/plural";
import { environmentsQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { showRefusalInline, useStoreWriter } from "#/modules/config-store/store-write";
import { useOrganizationName } from "#/modules/organization/organization-state.queries";
import { invalidatePreviews, usePreviewRule, usePreviews } from "#/modules/previews/previews.queries";
import { PreviewSignRefused, signPreviewEnd, signPreviewRule } from "#/modules/previews/preview-signing";
import { writeFailure } from "#/modules/previews/preview-writes";
import { PreviewsRefused, useServerName } from "./preview-parts";
import { startFromChoices } from "./start-from";

/** What Preview settings set: where previews copy from, the Servers they run on and how many are live at once. */
type Draft = { readonly startFrom: string | null; readonly servers: readonly string[] | null; readonly limit: number };

const LIMITS = [2, 3, 5, 8, 12];

/**
 * Preview settings for one repository: a start-from Environment, the Servers previews run on, and how many are live
 * at once. Turning them on, or an edit that changes what a preview copies or where it runs, is signed with a passkey;
 * an edit shows what changes first. Turning them off signs the rule's ending generation and closes live previews.
 */
export function PreviewSettingsPage({ params, repository, repositoryId, edit }: {
  params: PreviewRouteParams;
  repository: string;
  repositoryId: number;
  edit: boolean;
}) {
  const result = usePreviews(params.organizationSlug, { project: params.projectSlug, repository, number: null });
  return (
    <DashboardPage width="content" title="Preview settings" actions={
      <Button variant="outline" render={<Link to={PREVIEWS_ROUTE_TO} params={params} />}>Pull requests</Button>
    }>
      {!result.ok ? <PreviewsRefused message={result.refusal.message} />
        : <Settings key={result.value.settings?.generation ?? 0} view={result.value} params={params} repositoryId={repositoryId} edit={edit} />}
    </DashboardPage>
  );
}

function Settings({ view, params, repositoryId, edit }: { view: PreviewsView; params: PreviewRouteParams; repositoryId: number; edit: boolean }) {
  const organizationName = useOrganizationName(params.organizationSlug);
  const serverName = useServerName(params.organizationSlug);
  const navigate = useNavigate();
  const environments = useStoreView(params.organizationSlug, environmentsQuery(params.projectSlug));
  const choices = startFromChoices(environments.ok ? environments.value.environments : []);
  const settings = view.settings;
  const opening = (): Draft => settings
    ? { startFrom: settings.start_from, servers: settings.servers, limit: settings.limit }
    : { startFrom: choices.find((choice) => choice.name === params.environmentSlug && !choice.never)?.name ?? choices.find((choice) => !choice.never)?.name ?? null, servers: null, limit: view.limit };
  const [draft, setDraft] = useState<Draft | null>(edit ? opening() : null);
  const [signing, setSigning] = useState<"on" | "off" | null>(null);
  const shown = draft ?? (settings ? { startFrom: settings.start_from, servers: settings.servers, limit: settings.limit } : null);
  const rule = usePreviewRule(params.organizationSlug, shown?.startFrom
    ? { project: params.projectSlug, repository: view.repository, start_from: shown.startFrom } : null);
  const ruleView = rule.data?.ok ? rule.data.value : null;
  // Before any are picked, a new setup runs on every Server that can hold a preview.
  const servers = shown?.servers ?? ruleView?.servers.filter((server) => server.holds).map((server) => server.machine_id) ?? [];
  const stopEditing = () => {
    setDraft(null);
    if (edit) void navigate({ to: ".", search: {}, replace: true });
  };

  if (settings === null && draft === null) {
    return (
      <SettingsSection id="previews" title="Previews are off">
        <SettingsEmpty title="Deploy pull requests as previews" actions={<Button onClick={() => setDraft(opening())}>Set up previews</Button>}>
          Each one is a copy of an environment, at its own address.
        </SettingsEmpty>
      </SettingsSection>
    );
  }
  if (shown === null) return null;
  const changes = settings && draft ? changesOf(settings, { ...draft, servers }, serverName) : [];
  const resigns = settings === null || (draft !== null && (draft.startFrom !== settings.start_from || !sameServers(servers, settings.servers)));
  const update = (next: Partial<Draft>) => setDraft({ ...(draft ?? shown), servers, ...next });

  return (
    <>
      {settings && draft === null ? (
        <SettingsSection id="previews" title="Previews are on" action={<>
          <Button variant="outline" onClick={() => setSigning("off")}>Turn off</Button>
          <Button onClick={() => setDraft(opening())}>Edit</Button>
        </>}>
          <SettingsRow label={<Summary organizationName={organizationName} organizationSlug={params.organizationSlug} repository={view.repository} startFrom={settings.start_from}
            secrets={ruleView?.secrets.length} servers={servers} limit={settings.limit} />}
          hint={<>Set up <RelativeTime date={new Date(settings.issued_at)} />, version {settings.generation}</>} commit="none" />
        </SettingsSection>
      ) : null}
      <SettingsSection id="start-from" title="Each preview is a copy of">
        {choices.every((choice) => choice.never !== null) ? (
          <SettingsEmpty title="No environment to copy">Create one that isn't production or Protected, like staging.</SettingsEmpty>
        ) : <SettingsRow label="Start from" hint="Never production or a Protected environment: pull request code nobody has reviewed runs with its secrets."
          commit="form" control={
            <Select value={shown.startFrom} disabled={draft === null} onValueChange={(next) => { if (next) update({ startFrom: next, servers: null }); }}>
              <SelectTrigger aria-label="Each preview is a copy of"><SelectValue placeholder="Pick an environment">{shown.startFrom}</SelectValue></SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {choices.map((choice) => (
                    <SelectItem key={choice.name} value={choice.name} label={choice.name} disabled={choice.never !== null}>
                      {choice.never === null ? choice.name : `${choice.name}, ${choice.never}`}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          } />}
      </SettingsSection>
      <SettingsSection id="limit" title="Limit">
        <SettingsRow label="Live previews at once" hint="At the limit, a new deploy doesn't happen until someone closes a preview. Previews close when their pull request closes."
          commit="form" control={
            <Select value={String(shown.limit)} disabled={draft === null} onValueChange={(next) => { if (next) update({ limit: Number(next) }); }}>
              <SelectTrigger aria-label="Live previews at once"><SelectValue>Up to {shown.limit}</SelectValue></SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {[...new Set([...LIMITS, shown.limit])].sort((a, b) => a - b).map((limit) => (
                    <SelectItem key={limit} value={String(limit)} label={`Up to ${limit}`}>Up to {limit}</SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          } />
      </SettingsSection>
      <SettingsSection id="who" title="Who can deploy">
        <SettingsList label="Who can deploy">
          <SettingsListItem media={<UsersIcon />} label={`Members of ${organizationName}`} commit="none"
            hint={<>Deploy any pull request, and its later pushes redeploy it. To let a contributor deploy theirs, <Link to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug: params.organizationSlug }} search={{ section: "members" }} className="underline underline-offset-4">invite them</Link>.</>}
            actions={<Badge variant="success">One click</Badge>} />
          <SettingsListItem media={<KeyRoundIcon />} label="Anyone else's pull request" commit="none"
            hint="Coming soon: a member deploys one commit at a time with their passkey."
            actions={<Badge variant="secondary">Passkey per commit</Badge>} />
        </SettingsList>
      </SettingsSection>
      <SettingsSection id="servers" title="Where previews run">
        <Servers organizationSlug={params.organizationSlug} startFrom={shown.startFrom} rule={rule} ruleView={ruleView} picked={servers} editing={draft !== null} onPick={(next) => update({ servers: next })} />
      </SettingsSection>
      {draft !== null && changes.length > 0 ? (
        <SettingsSection id="changes" title="What changes" description="Live previews pick this up at their next deploy.">
          {changes.map((change) => <SettingsRow key={change} label={change} commit="none" />)}
        </SettingsSection>
      ) : null}
      {draft !== null ? (
        <div className="flex flex-wrap items-center justify-end gap-2">
          {resigns ? <span className="mr-auto text-sm text-muted-foreground">Your passkey confirms this once.</span> : null}
          <Button variant="outline" onClick={stopEditing}>Cancel</Button>
          <SaveButton view={view} params={params} draft={draft} servers={servers} ruleView={ruleView} resigns={resigns}
            settings={settings} onSign={() => setSigning("on")} onSaved={stopEditing} />
        </div>
      ) : null}
      {signing === "on" && draft !== null && ruleView !== null && draft.startFrom !== null ? (
        <SignSheet view={view} params={params} repositoryId={repositoryId} ruleView={ruleView} startFrom={draft.startFrom}
          servers={servers} limit={draft.limit} organizationName={organizationName} turningOn={settings === null}
          onClose={() => setSigning(null)} onSigned={() => { setSigning(null); stopEditing(); }} />
      ) : null}
      {signing === "off" && settings !== null ? (
        <TurnOffSheet view={view} params={params} ruleView={ruleView} onClose={() => setSigning(null)} />
      ) : null}
    </>
  );
}

const sameServers = (a: readonly string[], b: readonly string[]) => [...a].sort().join() === [...b].sort().join();

/** The edit's lines, each the setting and its value before and after. */
function changesOf(settings: PreviewSettings, draft: Draft & { servers: readonly string[] }, name: (id: string) => string) {
  const names = (ids: readonly string[]) => (ids.length ? listNames(ids.map(name)) : "none");
  const lines: string[] = [];
  if (draft.startFrom !== settings.start_from) lines.push(`Copy of ${settings.start_from} → ${draft.startFrom}`);
  if (draft.limit !== settings.limit) lines.push(`Up to ${settings.limit} → ${draft.limit} live previews`);
  if (!sameServers(draft.servers, settings.servers)) lines.push(`Runs on ${names(settings.servers)} → ${names(draft.servers)}`);
  return lines;
}

/** The settings in one sentence, as the person signed them. */
function Summary({ organizationName, organizationSlug, repository, startFrom, secrets, servers, limit }: {
  organizationName: string; organizationSlug: string; repository: string; startFrom: string; secrets: number | undefined;
  servers: readonly string[]; limit: number;
}) {
  const name = useServerName(organizationSlug);
  const names = servers.map(name);
  return (
    <span className="font-normal">
      Members of {organizationName} can deploy any pull request to {repository} as a copy of {startFrom}
      {secrets === undefined ? "" : `, with its ${plural(secrets, "secret")}`}
      {names.length ? `, on ${new Intl.ListFormat("en", { type: "disjunction" }).format(names)}` : ""}. Up to {limit} are live at once.
    </span>
  );
}

function Servers({ organizationSlug, startFrom, rule, ruleView, picked, editing, onPick }: {
  organizationSlug: string;
  startFrom: string | null;
  rule: ReturnType<typeof usePreviewRule>;
  ruleView: PreviewRuleView | null;
  picked: readonly string[];
  editing: boolean;
  onPick: (servers: readonly string[]) => void;
}) {
  const name = useServerName(organizationSlug);
  if (startFrom === null) return <SettingsEmpty title="Pick an environment to copy first" />;
  if (rule.data !== undefined && !rule.data.ok) return <SettingsEmpty title={`Can't copy ${startFrom}`}>{rule.data.refusal.message}</SettingsEmpty>;
  if (rule.isError) return <SettingsEmpty title="Couldn't read your servers" actions={<Button variant="outline" onClick={() => void rule.refetch()}>Retry</Button>} />;
  if (ruleView === null) {
    return (
      <div role="status" aria-label="Loading servers" className="flex flex-col gap-2 p-3">
        <Skeleton className="h-5 w-48" /><Skeleton className="h-5 w-40" />
      </div>
    );
  }
  if (ruleView.servers.length === 0) return <SettingsEmpty title="No server can hold a preview">Add a server, or upgrade yours.</SettingsEmpty>;
  return (
    <SettingsList label="Where previews run">
      {ruleView.servers.map((server) => {
        const on = picked.includes(server.machine_id);
        return (
          <SettingsListItem key={server.machine_id} pick={editing && server.holds} media={<ServerIcon />} label={name(server.machine_id)}
            hint={server.holds ? undefined : server.why} commit="form"
            actions={<Checkbox aria-label={name(server.machine_id)} checked={on} disabled={!editing || !server.holds}
              onCheckedChange={(next) => onPick(next ? [...picked, server.machine_id] : picked.filter((id) => id !== server.machine_id))} />} />
        );
      })}
    </SettingsList>
  );
}

/** Saves the edit: a limit alone saves at once; anything a preview copies or where it runs asks for a passkey. */
function SaveButton({ view, params, draft, servers, ruleView, resigns, settings, onSign, onSaved }: {
  view: PreviewsView; params: PreviewRouteParams; draft: Draft; servers: readonly string[]; ruleView: PreviewRuleView | null;
  resigns: boolean; settings: PreviewSettings | null; onSign: () => void; onSaved: () => void;
}) {
  const writer = useStoreWriter(params.organizationSlug);
  const { queryClient } = useCollectionScope();
  const unchanged = settings !== null && !resigns && draft.limit === settings.limit;
  const ready = draft.startFrom !== null && servers.length > 0 && ruleView !== null;
  if (resigns) {
    return (
      <Button disabled={!ready} onClick={onSign}>
        <KeyRoundIcon data-icon="inline-start" />
        {settings === null ? "Turn on with passkey" : "Save with passkey"}
      </Button>
    );
  }
  return (
    <Button disabled={unchanged} onClick={() => {
      void writer.commit({
        command: "set_pr_plan", project: params.projectSlug, repository: view.repository, enabled: null, start_from: null, copy: null,
        setup: null, remove_on_close: null, include_bots: null, preview_cap: draft.limit,
      }).then(() => { void invalidatePreviews(queryClient, params.organizationSlug); toast.success("Saved"); }, () => undefined);
      onSaved();
    }}>
      Save
    </Button>
  );
}

/** One line of what a passkey confirms: what it is, then the value. */
function Term({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Item size="xs" variant="muted">
      <ItemContent>
        <ItemTitle>{label}</ItemTitle>
        <ItemDescription className="line-clamp-none">{children}</ItemDescription>
      </ItemContent>
    </Item>
  );
}

/** Signs the preview rule with the person's passkey, after listing exactly what it confirms, then saves it. */
function SignSheet({ view, params, repositoryId, ruleView, startFrom, servers, limit, organizationName, turningOn, onClose, onSigned }: {
  view: PreviewsView; params: PreviewRouteParams; repositoryId: number; ruleView: PreviewRuleView; startFrom: string;
  servers: readonly string[]; limit: number; organizationName: string; turningOn: boolean; onClose: () => void; onSigned: () => void;
}) {
  const writer = useStoreWriter(params.organizationSlug);
  const { queryClient } = useCollectionScope();
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const name = useServerName(params.organizationSlug);
  const names = servers.map(name);
  const built = ruleView.sources.map((source) => source.service);
  const images = ruleView.images.map((image) => image.service);

  const sign = async () => {
    if (busy) return;
    setBusy(true);
    setFailed(null);
    try {
      const rule = await signPreviewRule({
        organizationSlug: params.organizationSlug, queryClient, view: ruleView, repository: view.repository, repositoryId, servers,
      });
      await save(rule);
      toast.success(turningOn ? "Previews are on" : "Saved", { description: turningOn ? "Each pull request now has a Deploy as a preview link." : "Live previews pick it up at their next deploy." });
      onSigned();
    } catch (error) {
      setFailed(error instanceof PreviewSignRefused ? error.message : writeFailure(error));
    } finally {
      setBusy(false);
    }
  };
  const save = async (rule: Signed) => {
    const write = writer.commit({
      command: "set_pr_plan", project: params.projectSlug, repository: view.repository, enabled: null, start_from: startFrom, copy: null,
      setup: null, remove_on_close: null, include_bots: null, preview_rule: rule, preview_cap: limit,
    });
    showRefusalInline(write);
    await write;
    void invalidatePreviews(queryClient, params.organizationSlug);
  };

  return (
    <Dialog open onOpenChange={(next) => { if (!next && !busy) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{turningOn ? "Turn on previews" : "Save preview settings"}</DialogTitle>
          <DialogDescription>Your servers check this themselves.</DialogDescription>
        </DialogHeader>
        <div className="flex max-h-72 flex-col gap-1 overflow-y-auto">
          <Term label="Who">Members of {organizationName} deploy pull requests to {view.repository}</Term>
          <Term label="Gets">
            A copy of {startFrom}{ruleView.secrets.length > 0
              ? <> with its secrets: <span className="font-mono ph-no-capture">{ruleView.secrets.join(", ")}</span></> : ", which has no secrets"}
          </Term>
          <Term label="Where">{listNames(names)}, up to {limit} live at once</Term>
          <Term label="Services">
            {[built.length ? `${listNames(built)} built from the pull request` : null, images.length ? `${listNames(images)} as in ${startFrom}` : null]
              .filter(Boolean).join("; ") || "None"}
          </Term>
          <Term label="Databases">Start empty</Term>
          <Term label="Not covered">Anyone else's pull request</Term>
        </div>
        {failed !== null ? <p role="alert" className="text-sm text-destructive">{failed}</p> : null}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy} />}>Cancel</DialogClose>
          <Button disabled={busy} onClick={() => void sign()}>
            {busy ? <Spinner data-icon="inline-start" /> : <KeyRoundIcon data-icon="inline-start" />}
            Use passkey
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** Turns previews off: the passkey signs the rule's ending generation, and the live previews close. */
function TurnOffSheet({ view, params, ruleView, onClose }: {
  view: PreviewsView; params: PreviewRouteParams; ruleView: PreviewRuleView | null; onClose: () => void;
}) {
  const writer = useStoreWriter(params.organizationSlug);
  const { queryClient } = useCollectionScope();
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const off = async () => {
    if (busy || ruleView === null) return;
    setBusy(true);
    setFailed(null);
    try {
      const rule = await signPreviewEnd(params.organizationSlug, ruleView);
      const write = writer.commit({
        command: "set_pr_plan", project: params.projectSlug, repository: view.repository, enabled: null, start_from: null, copy: null,
        setup: null, remove_on_close: null, include_bots: null, preview_rule: rule,
      });
      showRefusalInline(write);
      await write;
      void invalidatePreviews(queryClient, params.organizationSlug);
      toast.success("Previews are off", view.live > 0 ? { description: `${plural(view.live, "preview")} closing.` } : undefined);
      onClose();
    } catch (error) {
      setFailed(error instanceof PreviewSignRefused ? error.message : writeFailure(error));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(next) => { if (!next && !busy) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Turn off previews?</DialogTitle>
          <DialogDescription>Every live preview closes and its databases are deleted.</DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-1">
          <Term label="Closes">{view.live > 0 ? `${plural(view.live, "live preview")}, and your servers refuse to start them again` : "No preview is live"}</Term>
          <Term label="Pull requests">Stay open, and their Deploy as a preview link goes away</Term>
        </div>
        {ruleView === null ? <p className="text-sm text-muted-foreground">Reading your servers…</p> : null}
        {failed !== null ? <p role="alert" className="text-sm text-destructive">{failed}</p> : null}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy} />}>Cancel</DialogClose>
          <Button variant="destructive" disabled={busy || ruleView === null} onClick={() => void off()}>
            {busy ? <Spinner data-icon="inline-start" /> : <KeyRoundIcon data-icon="inline-start" />}
            Turn off with passkey
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
