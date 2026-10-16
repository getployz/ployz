import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { CheckIcon, ChevronRightIcon, ServerIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { useCommitHold } from "#/components/stageable/use-commit-hold";
import { Button } from "#/components/ui/button";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import type { BuildOrder } from "@ployz/sdk";
import { requireView, useStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { githubBuildRepositoryKey, githubBuildWorkflowUrl, type GithubBuildRepository } from "#/modules/github/github-build-workflow";
import { useGithubBuildRepositories, useGithubWebOrigin } from "#/modules/github/github.queries";
import { volumeSupportText } from "#/modules/machines/server-status";
import { useServerList, type ServerListItem } from "#/modules/machines/use-servers";

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
  return (
    <SettingsSection id="build-order" title="Where builds run">
      <SettingsRow
        label="Order"
        commit="change"
        control={(
          <Select value={buildOrder} onValueChange={(next) => {
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
  const { data: repositories, error } = useGithubBuildRepositories(organizationSlug, opened);
  const githubWeb = useGithubWebOrigin();
  const waiting = (repository: GithubBuildRepository) =>
    repository.readiness === "setup_needed" && opened.has(githubBuildRepositoryKey(repository));

  if (!repositories?.length && !error) return null;
  const ready = repositories?.filter((repository) => repository.readiness === "ready").length ?? 0;

  return (
    <SettingsSection id="github-actions" title="GitHub Actions" description={repositories
      ? `${ready} of ${repositories.length} repositories set up. Each needs one small workflow file. It only runs when Ployz starts a build, and build secrets are sent to the runner.`
      : "Could not check the repositories on GitHub."}>
      <SettingsList label="Repositories">
        {repositories?.map((repository) => (
          <SettingsListItem
            key={githubBuildRepositoryKey(repository)}
            media={<GitHubMarkIcon />}
            label={<span className="font-mono">{repository.fullName}</span>}
            hint={repository.readiness === "no_permission"
              ? "Installation lacks permission"
              : waiting(repository) ? "Waiting for commit" : undefined}
            commit="none"
            actions={repository.readiness === "ready" ? (
              <CheckIcon className="size-4 text-success" aria-label="Set up" />
            ) : repository.readiness === "no_permission" || waiting(repository) || repository.defaultBranch === null ? undefined : (
              <Button
                size="sm"
                variant="outline"
                nativeButton={false}
                render={
                  <a
                    href={githubBuildWorkflowUrl({ web: githubWeb, fullName: repository.fullName, defaultBranch: repository.defaultBranch })}
                    target="_blank"
                    rel="noreferrer"
                  />
                }
                onClick={() => setOpened((current) => new Set(current).add(githubBuildRepositoryKey(repository)))}
              >
                Add workflow ↗
              </Button>
            )}
          />
        ))}
      </SettingsList>
    </SettingsSection>
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
