import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { CheckIcon, ChevronRightIcon, ServerIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { CopyButton } from "#/components/copy-button";
import { SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { useCommitHold } from "#/components/stageable/use-commit-hold";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import type { BuildOrder } from "@ployz/sdk";
import { requireView, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import {
  awaitsWorkflowCommit,
  GITHUB_BUILD_WORKFLOW_FILE,
  GITHUB_WORKFLOW_TEMPLATES,
  githubBuildRepositoryKey,
  githubBuildWorkflowEditUrl,
  githubBuildWorkflowUrl,
  type GithubBuildRepository,
} from "#/modules/github/github-build-workflow";
import { useGithubBuildRepositories, useGithubWebOrigin } from "#/modules/github/github.queries";
import { volumeSupportText } from "#/modules/machines/server-status";
import { useServerList, type ServerListItem } from "#/modules/machines/use-servers";
import { useRoleGate } from "#/modules/organization/members.queries";

/** Where the Organization's Image Builds run: the Build Order, GitHub setup per repository, and which Servers build. */
export function BuildsSettings({ organizationSlug }: { organizationSlug: string }) {
  return (
    <div className="flex flex-col gap-6">
      <WhereBuildsRun organizationSlug={organizationSlug} />
      <GithubActions organizationSlug={organizationSlug} />
      <BuildServers organizationSlug={organizationSlug} />
    </div>
  );
}

const BUILD_ORDER_LABELS = {
  "github-then-servers": "GitHub, then your servers",
  "servers-then-github": "Your servers, then GitHub",
  "servers-only": "Your servers only",
  "github-only": "GitHub only",
} satisfies Record<BuildOrder, string>;
const BUILD_ORDERS: BuildOrder[] = ["github-then-servers", "servers-then-github", "servers-only", "github-only"];

export const BUILD_ORDER_REFUSED = "Only an owner or admin can change where builds run.";

/** The Organization's Build Order, which the settings page prefetches. */
export const BUILD_ORDER_QUERY = { query: "build_order" } as const;

/**
 * Takes effect on the next build; never staged. Never chosen, GitHub goes first, and a repository without the build
 * workflow skips it at once.
 */
function WhereBuildsRun({ organizationSlug }: { organizationSlug: string }) {
  const view = requireView(useStoreView(organizationSlug, BUILD_ORDER_QUERY));
  const writer = useStoreWriter(organizationSlug);
  // Shown at once; a refusal toasts and the saved order shows again.
  const [buildOrder, hold] = useCommitHold(view.build_order ?? "github-then-servers");
  // The Store refuses set_build_order without org:settings; the control says so first.
  const gate = useRoleGate(organizationSlug, "org:settings", BUILD_ORDER_REFUSED);
  return (
    <SettingsSection id="build-order" title="Where builds run">
      <SettingsRow
        label="Order"
        hint={gate.reason}
        commit="change"
        control={(
          <Select value={buildOrder} disabled={!gate.allowed} onValueChange={(next) => {
            const order = BUILD_ORDERS.find((candidate) => candidate === next);
            if (!order) return;
            hold(order, writer.commit({ command: "set_build_order", build_order: order }));
          }}>
            <SelectTrigger aria-label="Where builds run">
              <SelectValue>{BUILD_ORDER_LABELS[buildOrder]}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {BUILD_ORDERS.map((order) => (
                  <SelectItem key={order} value={order} label={BUILD_ORDER_LABELS[order]}>{BUILD_ORDER_LABELS[order]}</SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
        )}
      />
    </SettingsSection>
  );
}

/** GitHub Actions setup for the repositories this Organization's Services build from. Hidden when there are none. */
function GithubActions({ organizationSlug }: { organizationSlug: string }) {
  // ponytail: "waiting for the commit" lives in this tab only; persist it if teammates need to see it.
  const [opened, setOpened] = useState<ReadonlySet<string>>(new Set());
  const [updating, setUpdating] = useState<GithubBuildRepository | null>(null);
  const { data: repositories, error } = useGithubBuildRepositories(organizationSlug, opened);
  const githubWeb = useGithubWebOrigin();
  const waiting = (repository: GithubBuildRepository) =>
    awaitsWorkflowCommit(repository) && opened.has(githubBuildRepositoryKey(repository));
  const open = (repository: GithubBuildRepository) => setOpened((current) => new Set(current).add(githubBuildRepositoryKey(repository)));

  if (!repositories?.length && !error) return null;
  const ready = repositories?.filter((repository) => repository.readiness === "ready").length ?? 0;

  return (
    <SettingsSection id="github-actions" title="GitHub Actions" description={repositories
      ? `${ready} of ${repositories.length} repositories set up. Each needs one small workflow file. It runs when Ployz starts a build, or on each push under an automatic deploy rule. Build secrets are sent to the runner.`
      : "Could not check the repositories on GitHub."}>
      <SettingsList label="Repositories">
        {repositories?.map((repository) => (
          <SettingsListItem
            key={githubBuildRepositoryKey(repository)}
            media={<GitHubMarkIcon />}
            label={repository.fullName}
            hint={repository.readiness === "no_permission"
              ? "Installation lacks permission"
              : waiting(repository) ? "Waiting for commit"
              : repository.readiness === "outdated" ? "Uses an older workflow" : undefined}
            commit="none"
            actions={repository.readiness === "ready" ? (
              <CheckIcon className="size-4 text-success" aria-label="Set up" />
            ) : repository.readiness === "no_permission" || waiting(repository) || repository.defaultBranch === null ? undefined
            : repository.readiness === "outdated" ? (
              <Button size="sm" variant="outline" onClick={() => setUpdating(repository)}>Update workflow</Button>
            ) : (
              <Button
                size="sm"
                variant="outline"
                nativeButton={false}
                render={
                  <a
                    href={githubBuildWorkflowUrl({
                      web: githubWeb, fullName: repository.fullName, defaultBranch: repository.defaultBranch, template: repository.template,
                    })}
                    target="_blank"
                    rel="noreferrer"
                  />
                }
                onClick={() => open(repository)}
              >
                Add workflow ↗
              </Button>
            )}
          />
        ))}
      </SettingsList>
      {updating !== null && updating.defaultBranch !== null ? (
        <UpdateWorkflowDialog
          editUrl={githubBuildWorkflowEditUrl({ web: githubWeb, fullName: updating.fullName, defaultBranch: updating.defaultBranch })}
          defaultBranch={updating.defaultBranch}
          workflow={GITHUB_WORKFLOW_TEMPLATES[updating.template]}
          onOpened={() => {
            open(updating);
            setUpdating(null);
          }}
          onClose={() => setUpdating(null)}
        />
      ) : null}
    </SettingsSection>
  );
}

/** The new file to copy, and GitHub's editor on the old one: Cloud never writes to the repository. */
function UpdateWorkflowDialog({ editUrl, defaultBranch, workflow, onOpened, onClose }: {
  editUrl: string; defaultBranch: string; workflow: string; onOpened: () => void; onClose: () => void;
}) {
  return (
    <Dialog open onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Update workflow</DialogTitle>
          <DialogDescription>
            Replace <span className="font-mono">.github/workflows/{GITHUB_BUILD_WORKFLOW_FILE}</span> on{" "}
            <span className="font-mono">{defaultBranch}</span> with this file. It runs Ployz's builder at a fixed commit, so a change to the builder can't reach your builds.
          </DialogDescription>
        </DialogHeader>
        <pre aria-label="New workflow file" className="max-h-80 overflow-auto rounded-md border bg-muted/40 p-3 font-mono text-xs leading-5">
          {workflow}
        </pre>
        <DialogFooter>
          <CopyButton value={workflow} label="Copy file" showLabel variant="outline" />
          <Button nativeButton={false} render={<a href={editUrl} target="_blank" rel="noreferrer" />} onClick={onOpened}>
            Open in GitHub ↗
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function buildSummary({ machine }: ServerListItem) {
  if (!machine.acceptsBuilds) return "Doesn’t run builds";
  if (machine.runningBuilds > 0) return `Building ${machine.runningBuilds} now`;
  return `Up to ${machine.effectiveBuildConcurrency} at once`;
}

/** Which Servers take builds, read-only: each Server's own page holds its switch. */
function BuildServers({ organizationSlug }: { organizationSlug: string }) {
  const { servers } = useServerList(organizationSlug);
  if (servers.length === 0) return null;
  const building = servers.filter((server) => server.machine.acceptsBuilds).length;
  return (
    <SettingsSection id="build-servers" title="Your servers"
      description={`${building} of ${servers.length} run builds. Turn builds on or off on each server’s page.`}>
      <SettingsList label="Servers">
        {servers.map((server) => (
          <SettingsListItem
            key={server.machine.id}
            media={<ServerIcon />}
            label={server.name}
            hint={(
              <>
                {buildSummary(server)} · {volumeSupportText(server.machine.storage)}
                {server.machine.daemonVersion ? <> · <span className="font-mono">{server.machine.daemonVersion}</span></> : null}
              </>
            )}
            commit="none"
            actions={(
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={`Open ${server.name}`}
                nativeButton={false}
                render={<Link to="/cloud/$organizationSlug/~/servers/$serverId" params={{ organizationSlug, serverId: server.machine.id }} />}
              >
                <ChevronRightIcon />
              </Button>
            )}
          />
        ))}
      </SettingsList>
    </SettingsSection>
  );
}
