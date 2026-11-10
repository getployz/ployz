import { useState } from "react";
import type { RuleDraft, RuleView, Signed } from "@ployz/sdk";
import { useQuery } from "@tanstack/react-query";
import { useLoaderData } from "@tanstack/react-router";
import { KeyRoundIcon, TriangleAlertIcon } from "lucide-react";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { SettingsEmpty } from "#/components/settings/settings-list";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { deployScope, RULE_CAPABILITIES } from "#/modules/approval-session/approval-session";
import { ruleQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { answerRule, draftEnd, draftRule, keptRule, RuleRefused, ruleOn } from "#/modules/github/auto-deploy-rule";
import { githubRepositoryOwnerQueryOptions } from "#/modules/github/github.queries";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useEnvironmentMachines } from "#/providers/runtime-provider";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

type Place = { organizationSlug: string; projectSlug: string; environmentSlug: string };

const listed = (names: readonly string[]) =>
  names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;

/** Where a rule goes: the Cluster and every Server in it, and the first Server that enforces but can't take one. */
function useRuleScope(organizationSlug: string, environment: { environmentName: string; namespace: string }) {
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { machines } = useRuntimeLens(organizationSlug);
  const environmentMachines = useEnvironmentMachines(environment.namespace);
  return deployScope({ environmentId, ...environment }, environmentMachines, machines, RULE_CAPABILITIES);
}

/**
 * Deploy every push of the Environment's branch, on Servers that enforce device approval, under a rule the person's
 * passkey approves with a prompt of its own. A Saved change to what the rule covers holds pushes until it is approved
 * again, which shows here as a card.
 */
export function StoreAutomaticDeploys({ organizationSlug, projectSlug, environmentSlug }: Place) {
  const environment = { project: projectSlug, environment: environmentSlug };
  const read = useStoreView(organizationSlug, ruleQuery(environment));
  const view = read.ok ? read.value : null;
  const rule = useRuleScope(organizationSlug, { environmentName: environmentSlug, namespace: view?.namespace ?? "" });
  const [open, setOpen] = useState<"approve" | "end" | null>(null);
  const writer = useStoreWriter(organizationSlug);
  if (view === null) return null;
  const kept = keptRule(view);
  const on = ruleOn(view);
  const held = on ? view.changed : [];
  const branch = kept?.source.branch ?? view.sources[0]?.branch ?? "your branch";
  const covered = kept?.services.map((service) => service.name) ?? [];
  return (
    <SettingsSection id="automatic-deploys" title="Automatic deploys"
      description="For servers that enforce device approval.">
      {held.length > 0 ? (
        <SettingsRow
          label={<span className="flex items-center gap-2"><TriangleAlertIcon className="size-4 shrink-0 text-warning" />Pushes to {branch} are on hold</span>}
          hint={`${listed(held)} changed since you approved.`}
          commit="none"
          control={<Button onClick={() => setOpen("approve")}>Review and approve</Button>}
        />
      ) : null}
      {!on && view.sources.length === 0 ? (
        <SettingsEmpty title="No service builds from GitHub">Connect a service to a repository first.</SettingsEmpty>
      ) : (
        <SettingsRow
          label="Deploy every push"
          hint={on ? `Pushes to ${branch} deploy ${listed(covered)}.`
            : rule.enforced ? `Approve once with your passkey. Pushes to ${branch} then deploy with no prompt.`
            : "Your servers don't enforce device approval, so pushes already deploy."}
          commit="none"
          control={on || rule.enforced ? (
            <Switch id="automatic-deploys" aria-label="Deploy every push" checked={on}
              onCheckedChange={(next) => setOpen(next ? "approve" : "end")} />
          ) : null}
        />
      )}
      {open === null ? null : (
        <RuleDialog organizationSlug={organizationSlug} view={view} scope={rule} ending={open === "end"}
          onClose={() => setOpen(null)}
          commit={async (signed) => { await writer.commit({ command: "approve_rule", environment, rule: signed }); }}
          ownerQuery={githubRepositoryOwnerQueryOptions} />
      )}
    </SettingsSection>
  );
}

/**
 * What turning a rule on, approving it again or turning it off asks: the rule as it would be signed, then one passkey
 * prompt. `commit` keeps the signed rule in the Store; `ownerQuery` reads who owns the repository on GitHub.
 */
export function RuleDialog({ organizationSlug, view, scope, ending, onClose, commit, ownerQuery }: {
  organizationSlug: string;
  view: RuleView;
  scope: ReturnType<typeof deployScope>;
  ending: boolean;
  onClose: () => void;
  commit: (rule: Signed) => Promise<void>;
  ownerQuery: typeof githubRepositoryOwnerQueryOptions;
}) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const blocked = ending ? null
    : scope.older !== null ? `${scope.older} needs a newer Ployz for automatic deploys from the dashboard. Upgrade it, or run ployz github rule approve.`
    : scope.scope === null ? "Cloud couldn't check the servers. Try again."
    : null;
  // What the dialog shows: the rule as it would be signed now. The press drafts it again, at its own moment.
  const target = scope.scope;
  let preview: RuleDraft | null = null;
  let unready: string | null = null;
  try {
    preview = ending ? draftEnd(view) : target === null ? null : draftRule(view, target, 0);
  } catch (error) {
    unready = error instanceof RuleRefused ? error.message : "This browser couldn't draft the rule. Try again.";
  }
  const repository = ending ? null : preview?.summary.repository ?? null;
  // The button waits, disabled, until GitHub answers: a read shows nothing in flight.
  const owner = useQuery({ ...ownerQuery(organizationSlug, repository ?? ""), enabled: repository !== null });
  const ownerProblem = repository === null || owner.isPending ? null
    : owner.isError ? `Cloud couldn't read ${repository} on GitHub. Try again.`
    : owner.data.repositoryId !== view.sources.find((source) => source.repository === repository)?.repository_id
      ? `${repository} now names another repository. Connect your services to it again, then approve.` : null;
  const problem = blocked ?? unready ?? ownerProblem;
  const ready = preview !== null && problem === null && (ending || owner.data !== undefined);

  const approve = async () => {
    if (busy || !ready || (!ending && (target === null || owner.data === undefined))) return;
    setBusy(true);
    setFailed(null);
    try {
      const draft = ending || target === null ? draftEnd(view) : draftRule(view, target, owner.data?.ownerId ?? 0);
      const rule = await answerRule(draft);
      await commit(rule);
      onClose();
    } catch (error) {
      // The Store's refusal toasts by itself; a passkey's or the draft's shows here.
      if (error instanceof RuleRefused) setFailed(error.message);
    } finally {
      setBusy(false);
    }
  };

  const summary = preview?.summary;
  return (
    <Dialog open onOpenChange={(next) => { if (!next && !busy) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{ending ? "Turn off automatic deploys?" : ruleOn(view) ? "Approve automatic deploys again" : "Turn on automatic deploys"}</DialogTitle>
          <DialogDescription>
            {ending
              ? `Pushes to ${summary?.branch ?? "the branch"} stop deploying.`
              : `Every push to ${summary?.branch ?? "the branch"} deploys with these settings. A change to them waits for you.`}
          </DialogDescription>
        </DialogHeader>
        {summary && !ending ? <RuleSummaryList summary={summary} /> : null}
        {problem !== null ? <p role="alert" className="text-sm text-destructive">{problem}</p>
          : failed !== null ? <p role="alert" className="text-sm text-destructive">{failed}</p> : null}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy} />}>Cancel</DialogClose>
          <Button disabled={busy || !ready} onClick={() => void approve()}>
            {busy ? <Spinner data-icon="inline-start" /> : <KeyRoundIcon data-icon="inline-start" />}
            {ending ? "Turn off with passkey" : "Approve with passkey"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** What the passkey approves, line by line, as the CLI prints it before it signs. */
function RuleSummaryList({ summary }: { summary: RuleDraft["summary"] }) {
  return (
    <dl className="flex flex-col gap-3 text-sm">
      <div className="flex flex-col gap-1">
        <dt className="text-muted-foreground">Repository</dt>
        <dd><span className="font-mono">{summary.repository}</span> on <span className="font-mono">{summary.branch}</span></dd>
      </div>
      <div className="flex flex-col gap-1">
        <dt className="text-muted-foreground">Workflow</dt>
        <dd><span className="font-mono">{summary.workflow}</span>, on push</dd>
      </div>
      <div className="flex flex-col gap-1">
        <dt className="text-muted-foreground">Builder</dt>
        <dd className="break-all">{summary.pinned ? "Ployz's pinned release" : <span className="font-mono">{summary.builder}</span>}</dd>
      </div>
      <div className="flex flex-col gap-1">
        <dt className="text-muted-foreground">Servers</dt>
        <dd>{summary.servers} in the cluster</dd>
      </div>
      {summary.services.map((service) => (
        <div key={service.name} className="flex flex-col gap-1">
          <dt className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{service.name}</span>
            {service.changes.length > 0 ? <Badge variant="warning">Changed: {service.changes.join(", ")}</Badge> : null}
          </dt>
          <dd className="text-muted-foreground">{service.builds}</dd>
          {service.settings.map((line) => <dd key={line} className="font-mono text-xs break-all">{line}</dd>)}
        </div>
      ))}
    </dl>
  );
}
