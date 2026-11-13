import { useState } from "react";
import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { getRouteApi } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { getClusterDomainCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { checkClusterDomainNowServerFn } from "#/modules/cluster-domain/cluster-domain.functions";
import { type ClusterNameView, clusterNameProblem, defaultClusterName } from "#/modules/cluster-name/cluster-name";
import { useClusterName } from "#/modules/cluster-name/cluster-name.hooks";
import { useClusterIngress } from "#/modules/ingress/doors.queries";
import { useRoleGate } from "#/modules/organization/members.queries";
import { WebTrafficSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/WebTrafficSection";
import { ClusterDomainSection } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection";

const organizationRoute = getRouteApi("/_protected/cloud/$organizationSlug");

export function NetworkSection({ organizationSlug }: { organizationSlug: string }) {
  const { setup } = useClusterIngress(organizationSlug);
  return (
    <div className="flex flex-col gap-6">
      <ClusterNameSection organizationSlug={organizationSlug} />
      {setup?.state === "active" ? null : <ClusterDomainSettings organizationSlug={organizationSlug} />}
      <WebTrafficSection organizationSlug={organizationSlug} />
    </div>
  );
}

function ClusterNameSection({ organizationSlug }: { organizationSlug: string }) {
  const { view, rename } = useClusterName(organizationSlug);
  const [draft, setDraft] = useState<string | null>(null);
  const { access } = organizationRoute.useLoaderData();
  const gate = useRoleGate(organizationSlug, "servers:operate");
  const editable = access !== "support" && gate.allowed;
  return (
    <SettingsSection id="cluster" title="Cluster">
      {draft === null ? (
        <SettingsRow
          label="Cluster name"
          hint={hintOf(view)}
          commit="none"
          control={(
            <>
              {view.name === null ? <span className="text-sm text-muted-foreground">Not set</span> : <span className="font-mono text-sm break-all">{view.name}</span>}
              {view.state === "pending" ? <Badge variant="info">Pending</Badge> : view.state === "refused" ? <Badge variant="warning">Not applied</Badge> : null}
              {editable ? (
                <Button type="button" variant="outline" size="sm" onClick={() => setDraft(view.name ?? defaultClusterName(organizationSlug))}>Edit</Button>
              ) : null}
            </>
          )}
        />
      ) : (
        <ClusterNameForm
          current={view.name}
          draft={draft}
          onDraft={setDraft}
          onCancel={() => setDraft(null)}
          onSave={(name) => {
            rename(name);
            setDraft(null);
          }}
        />
      )}
    </SettingsSection>
  );
}

function hintOf(view: ClusterNameView) {
  switch (view.state) {
    case "current": return <span className="font-mono">db.prod.{view.name}.ployz.internal</span>;
    case "pending": return "Applies when a server is online.";
    case "refused": return view.refusal;
    case "unset": return undefined;
  }
}

function ClusterNameForm({ current, draft, onDraft, onCancel, onSave }: {
  current: string | null;
  draft: string;
  onDraft: (draft: string) => void;
  onCancel: () => void;
  onSave: (name: string) => void;
}) {
  const problem = clusterNameProblem(draft);
  const savable = problem === null && draft !== current;
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (savable) onSave(draft);
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") onCancel();
      }}
    >
      <SettingsRow
        label="Cluster name"
        hint={problem === null ? <span className="font-mono">db.prod.{draft}.ployz.internal</span> : undefined}
        commit="form"
        control={(
          <div className="flex w-full gap-2">
            <Input aria-label="Cluster name" value={draft} autoFocus spellCheck={false} autoComplete="off" aria-invalid={problem !== null}
              aria-describedby={problem === null ? undefined : "cluster-name-problem"} className="font-mono" onChange={(event) => onDraft(event.target.value)} />
            <Button type="submit" size="sm" disabled={!savable}>Save</Button>
            <Button type="button" size="sm" variant="ghost" onClick={onCancel}>Cancel</Button>
          </div>
        )}
      >
        {problem === null ? null : <p id="cluster-name-problem" className="text-sm text-destructive">{problem}</p>}
      </SettingsRow>
    </form>
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
