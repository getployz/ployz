import { useState } from "react";
import { CheckIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { BUILD_ORDERS, BUILD_ORDER_LABELS, defaultBuildOrder } from "#/modules/deployments/build-order";
import { useBuildOrder } from "#/modules/deployments/build-order.collection";
import { githubBuildRepositoryKey, githubBuildWorkflowUrl, type GithubBuildRepository } from "#/modules/github/github-build-workflow";
import { useGithubBuildRepositories } from "#/modules/github/github.queries";
import { useServerList, type ServerListItem } from "#/modules/machines/use-servers";
import { ServerLinkItem } from "./server-link-item";

/** Where the Organization's Image Builds run: the Build Order, GitHub setup per repository, and which Servers build. */
export function BuildsSettings({ organizationSlug }: { organizationSlug: string }) {
  return (
    <div className="flex flex-col gap-8">
      <WhereBuildsRun organizationSlug={organizationSlug} />
      <GithubActions organizationSlug={organizationSlug} />
      <BuildServers organizationSlug={organizationSlug} />
    </div>
  );
}

/** Takes effect on the next build; never staged. */
function WhereBuildsRun({ organizationSlug }: { organizationSlug: string }) {
  const { buildOrder: chosen, setBuildOrder } = useBuildOrder(organizationSlug);
  // Never chosen: the default follows whether GitHub is set up, as Cloud decides it at each build.
  const { data: repositories } = useGithubBuildRepositories(organizationSlug);
  const buildOrder = chosen ?? defaultBuildOrder(repositories?.some(({ readiness }) => readiness === "ready") ?? false);
  return (
    <section aria-labelledby="build-order-heading">
      <ItemGroup>
        <ItemContent>
          <ItemTitle>
            <h2 id="build-order-heading">Where builds run</h2>
          </ItemTitle>
          <ItemDescription>Builds try these in order and move on when one can’t start in time. A service can prefer one in its own settings.</ItemDescription>
        </ItemContent>
        <Select value={buildOrder} onValueChange={(next) => {
          const order = BUILD_ORDERS.find((candidate) => candidate === next);
          if (order) setBuildOrder(order);
        }}>
          <SelectTrigger aria-label="Where builds run" className="w-full sm:w-72">
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
      </ItemGroup>
    </section>
  );
}

/** GitHub Actions setup for the repositories this Organization's Services build from. Hidden when there are none. */
function GithubActions({ organizationSlug }: { organizationSlug: string }) {
  // ponytail: "waiting for the commit" lives in this tab only; persist it if teammates need to see it.
  const [opened, setOpened] = useState<ReadonlySet<string>>(new Set());
  const { data: repositories, error } = useGithubBuildRepositories(organizationSlug, opened);
  const waiting = (repository: GithubBuildRepository) =>
    repository.readiness === "setup_needed" && opened.has(githubBuildRepositoryKey(repository));

  if (!repositories?.length && !error) return null;
  const ready = repositories?.filter((repository) => repository.readiness === "ready").length ?? 0;

  return (
    <section aria-labelledby="github-actions-heading">
      <ItemGroup>
        <ItemContent>
          <ItemTitle>
            <h2 id="github-actions-heading">GitHub Actions</h2>
          </ItemTitle>
          <ItemDescription>
            {repositories
              ? `${ready} of ${repositories.length} repositories set up. Each needs one small workflow file. It only runs when Ployz starts a build, and build secrets are sent to the runner.`
              : "Could not check the repositories on GitHub."}
          </ItemDescription>
        </ItemContent>
        {repositories?.map((repository) => (
          <Item key={githubBuildRepositoryKey(repository)} variant="outline" size="sm">
            <ItemMedia variant="icon"><GitHubMarkIcon /></ItemMedia>
            <ItemContent className="min-w-0">
              <ItemTitle className="font-mono">{repository.fullName}</ItemTitle>
            </ItemContent>
            <ItemActions>
              {repository.readiness === "ready" ? (
                <CheckIcon className="size-4 text-success" aria-label="Set up" />
              ) : repository.readiness === "no_permission" ? (
                <ItemDescription>Installation lacks permission</ItemDescription>
              ) : waiting(repository) ? (
                <ItemDescription>Waiting for commit</ItemDescription>
              ) : repository.defaultBranch === null ? null : (
                <Button
                  size="xs"
                  variant="ink"
                  nativeButton={false}
                  render={
                    <a
                      href={githubBuildWorkflowUrl({ fullName: repository.fullName, defaultBranch: repository.defaultBranch })}
                      target="_blank"
                      rel="noreferrer"
                    />
                  }
                  onClick={() => setOpened((current) => new Set(current).add(githubBuildRepositoryKey(repository)))}
                >
                  Add workflow ↗
                </Button>
              )}
            </ItemActions>
          </Item>
        ))}
      </ItemGroup>
    </section>
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
    <section aria-labelledby="build-servers-heading">
      <ItemGroup>
        <ItemContent>
          <ItemTitle>
            <h2 id="build-servers-heading">Your servers</h2>
          </ItemTitle>
          <ItemDescription>{building} of {servers.length} run builds. Turn builds on or off on each server’s page.</ItemDescription>
        </ItemContent>
        {servers.map((server) => (
          <ServerLinkItem key={server.machine.id} organizationSlug={organizationSlug} server={server} description={buildSummary(server)} />
        ))}
      </ItemGroup>
    </section>
  );
}
