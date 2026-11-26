import { useState } from "react";
import type { RuleView, Signed } from "@ployz/sdk";
import type { PushBranch, RegistrySource } from "@ployz/sdk/authority";
import { KeyRoundIcon, TriangleAlertIcon } from "lucide-react";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { SettingsEmpty } from "#/components/settings/settings-list";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { diffQuery, ruleQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { useSaveTap } from "#/modules/deploy-approval/save-sheet";
import { priorOf, sourceName } from "#/modules/deploy-approval/source-step";
import { answerRule, draftEnd, keptRule, RuleRefused, ruleOn } from "./auto-deploy-rule";

type Place = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/** A branch a pushed Service builds from, with its repository's name. */
type Branch = PushBranch & { readonly repository: string };

const listed = (names: readonly string[]) =>
  names.length <= 1 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;

const keyOf = (branch: PushBranch) => `${branch.repository_id}:${branch.branch}`;

/** A registry repository Services run images from, with those Services. */
type Repository = { readonly registry: RegistrySource; readonly name: string; readonly services: string[] };

/** Each registry repository a Saved Service runs an image from, once, as the Store names it. */
function imageRepositories(view: RuleView): Repository[] {
  const found = new Map<string, Repository>();
  for (const image of view.images ?? []) {
    if (image.registry === undefined) continue;
    const name = `${image.registry.host}/${image.registry.repository}`;
    const repository = found.get(name) ?? { registry: image.registry, name, services: [] };
    repository.services.push(image.service);
    found.set(name, repository);
  }
  return [...found.values()];
}

/** Each branch a Service with Auto-deploy on builds from, once. */
function pushBranches(view: RuleView): Branch[] {
  const found = new Map<string, Branch>();
  for (const source of view.sources) {
    if (!source.on_push || !source.branch) continue;
    const branch = { repository: source.repository, repository_id: source.repository_id, branch: source.branch };
    found.set(keyOf(branch), branch);
  }
  return [...found.values()];
}

/**
 * Deploy every push of one branch, or every image pushed to one registry repository, under a rule the person's passkey
 * signs. Turning it on is a Save that signs the rule in the same tap; a Save or Deploy that changes what it covers signs
 * it again. With Services on several branches, the person picks the one whose pushes deploy. An Environment keeps one
 * rule, so turning on one source turns off the other.
 */
export function StoreAutomaticDeploys({ organizationSlug, projectSlug, environmentSlug }: Place) {
  const environment = { project: projectSlug, environment: environmentSlug };
  const read = useStoreView(organizationSlug, ruleQuery(environment));
  const diff = useStoreView(organizationSlug, diffQuery(environment));
  const saveTap = useSaveTap(organizationSlug);
  const writer = useStoreWriter(organizationSlug);
  const [ending, setEnding] = useState(false);
  const [picked, setPicked] = useState<string | null>(null);
  if (!read.ok) return null;
  const view = read.value;
  const kept = keptRule(view);
  const keptBranch = kept !== null && "branch" in kept.source ? kept.source : null;
  const keptImages = kept !== null && !("branch" in kept.source) ? kept.source : null;
  // `on` is whether pushes deploy; images deploy under a registry rule instead.
  const on = ruleOn(view) && keptBranch !== null;
  const imagesOn = ruleOn(view) && keptImages !== null;
  const branches = pushBranches(view);
  const repositories = imageRepositories(view);
  const following = keptBranch === null ? undefined
    : branches.find((branch) => branch.repository_id === keptBranch.repository_id && branch.branch === keptBranch.branch);
  // While on, the picker shows the rule's branch, and picking another turns the rule on there.
  const selected = on ? following : branches.find((branch) => keyOf(branch) === picked) ?? following ?? branches[0];
  // Signing the rule saves what is staged, so it waits for a read that shows nothing is, and names that review: the
  // Store refuses the Save if anyone stages since.
  const clean = diff.ok && diff.value.holds_staged ? diff.value.version : null;
  const staged = diff.ok && !diff.value.holds_staged;
  const held = on || imagesOn ? view.changed : [];
  const shownBranch = on && keptBranch !== null ? keptBranch.branch : selected?.branch ?? "your branch";
  const githubRepositories = new Set(branches.map((branch) => branch.repository_id));
  const named = (branch: Branch) => githubRepositories.size > 1 ? `${branch.repository} at ${branch.branch}` : branch.branch;
  const turnOn = ({ repository_id, branch }: Branch) => {
    if (clean === null) return;
    // Whatever stopped it already showed; the switch stays as the Store keeps the rule.
    saveTap.save({ command: "publish", environment, version: clean, accept_volume_loss: [], message: null }, [], { repository_id, branch, shown: priorOf(view) })
      .catch(() => undefined);
  };
  const turnOnImages = (registry: RegistrySource) => {
    if (clean === null) return;
    saveTap.save({ command: "publish", environment, version: clean, accept_volume_loss: [], message: null }, [], { registry, shown: priorOf(view) })
      .catch(() => undefined);
  };
  const deploysImages = (repository: Repository) => imagesOn && keptImages !== null
    && keptImages.host === repository.registry.host && keptImages.repository === repository.registry.repository;
  return (
    <SettingsSection id="automatic-deploys" title="Automatic deploys"
      description="Deploy each push or new image without a prompt.">
      {imagesOn && keptImages !== null ? (
        <SettingsRow
          label={held.length > 0
            ? <span className="flex items-center gap-2"><TriangleAlertIcon className="size-4 shrink-0 text-warning" />New images from {sourceName(keptImages)} are on hold</span>
            : "Approval"}
          hint={held.length > 0 ? `${listed(held)} changed since you approved.`
            : staged ? "Save or discard your changes first."
            : "Approve again after you add a server or a deploy is refused."}
          commit="none"
          control={(
            <Button variant={held.length > 0 ? "default" : "outline"} disabled={clean === null} onClick={() => turnOnImages(keptImages)}>
              <KeyRoundIcon data-icon="inline-start" />
              Approve again
            </Button>
          )}
        />
      ) : null}
      {repositories.map((repository) => (
        <SettingsRow
          key={repository.name}
          label={<>Deploy every image pushed to <span className="font-mono">{repository.name}</span></>}
          hint={deploysImages(repository)
            ? `Deploying a new digest of ${listed(repository.services)} needs no prompt.`
            : staged ? "Save or discard your changes first."
            : `Approve once with your passkey. Then deploying a new digest of ${listed(repository.services)} needs no prompt.`}
          commit="none"
          control={
            <Switch aria-label={`Deploy every image pushed to ${repository.name}`} checked={deploysImages(repository)}
              disabled={!deploysImages(repository) && clean === null}
              onCheckedChange={(next) => {
                if (!next) setEnding(true);
                else turnOnImages(repository.registry);
              }} />
          }
        />
      ))}
      {on && following !== undefined ? (
        <SettingsRow
          label={held.length > 0
            ? <span className="flex items-center gap-2"><TriangleAlertIcon className="size-4 shrink-0 text-warning" />Pushes to {shownBranch} are on hold</span>
            : "Approval"}
          hint={held.length > 0 ? `${listed(held)} changed since you approved.`
            : staged ? "Save or discard your changes first."
            : "Approve again after you add a server or a push is refused."}
          commit="none"
          control={(
            <Button variant={held.length > 0 ? "default" : "outline"} disabled={clean === null} onClick={() => turnOn(following)}>
              <KeyRoundIcon data-icon="inline-start" />
              Approve again
            </Button>
          )}
        />
      ) : null}
      {!on && branches.length === 0 ? (
        repositories.length > 0 ? null
        : view.sources.length === 0
          ? <SettingsEmpty title="No service builds from GitHub">Connect a service to a repository first.</SettingsEmpty>
          : <SettingsEmpty title="No service deploys on push">Turn on Auto-deploy for a service first.</SettingsEmpty>
      ) : (
        <>
          {branches.length > 1 ? (
            <SettingsRow
              label="Branch"
              hint="Only pushes to this branch deploy."
              commit="change"
              control={(
                <Select value={selected === undefined ? null : keyOf(selected)} disabled={on && clean === null} onValueChange={(next) => {
                  const branch = branches.find((candidate) => keyOf(candidate) === next);
                  if (branch === undefined) return;
                  if (on) turnOn(branch);
                  else setPicked(keyOf(branch));
                }}>
                  <SelectTrigger aria-label="Branch that deploys on push">
                    <SelectValue>{selected === undefined ? "Pick a branch" : named(selected)}</SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    <SelectGroup>
                      {branches.map((branch) => (
                        <SelectItem key={keyOf(branch)} value={keyOf(branch)} label={named(branch)}>{named(branch)}</SelectItem>
                      ))}
                    </SelectGroup>
                  </SelectContent>
                </Select>
              )}
            />
          ) : null}
          <SettingsRow
            label="Deploy every push"
            hint={on ? `Pushes to ${shownBranch} deploy ${listed(kept?.services.map((service) => service.name) ?? [])}.`
              : staged ? "Save or discard your changes first."
              : `Approve once with your passkey. Pushes to ${shownBranch} then deploy with no prompt.`}
            commit="none"
            control={
              <Switch id="automatic-deploys" aria-label="Deploy every push" checked={on}
                disabled={!on && (clean === null || selected === undefined)}
                onCheckedChange={(next) => {
                  if (!next) setEnding(true);
                  else if (selected !== undefined) turnOn(selected);
                }} />
            }
          />
        </>
      )}
      {ending ? (
        <EndDialog view={view} stops={keptImages !== null ? `New images from ${sourceName(keptImages)} stop deploying.` : `Pushes to ${shownBranch} stop deploying.`} onClose={() => setEnding(false)}
          commit={async (rule) => { await writer.commit({ command: "approve_rule", environment, rule }); }} />
      ) : null}
      {saveTap.sheet}
    </SettingsSection>
  );
}

/** What turning the rule off asks: one passkey prompt for its ending generation, which `commit` keeps in the Store. */
function EndDialog({ view, stops, onClose, commit }: {
  view: RuleView;
  stops: string;
  onClose: () => void;
  commit: (rule: Signed) => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const end = async () => {
    if (busy) return;
    setBusy(true);
    setFailed(null);
    try {
      await commit(await answerRule(draftEnd(view)));
      onClose();
    } catch (error) {
      // The Store's refusal toasts by itself; a passkey's or the draft's shows here.
      if (error instanceof RuleRefused) setFailed(error.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open onOpenChange={(next) => { if (!next && !busy) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Turn off automatic deploys?</DialogTitle>
          <DialogDescription>{stops}</DialogDescription>
        </DialogHeader>
        {failed !== null ? <p role="alert" className="text-sm text-destructive">{failed}</p> : null}
        <DialogFooter>
          <DialogClose render={<Button variant="outline" disabled={busy} />}>Cancel</DialogClose>
          <Button disabled={busy} onClick={() => void end()}>
            {busy ? <Spinner data-icon="inline-start" /> : <KeyRoundIcon data-icon="inline-start" />}
            Turn off with passkey
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
