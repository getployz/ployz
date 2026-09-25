import { useState } from "react";
import { CheckIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Card, CardAction, CardDescription, CardHeader, CardTitle } from "#/components/ui/card";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "#/components/ui/dialog";
import { githubBuildRepositoryKey, githubBuildWorkflowUrl, type GithubBuildRepository } from "#/modules/github/github-build-workflow";
import { useGithubBuildRepositories } from "#/modules/github/github.queries";

/** GitHub Actions setup for the repositories this organization's Services build from. Hidden when there are none. */
export function GithubBuildsCard({ organizationSlug }: { organizationSlug: string }) {
  // ponytail: "waiting for the commit" lives in this tab only; persist it if teammates need to see it.
  const [opened, setOpened] = useState<ReadonlySet<string>>(new Set());
  const { data: repositories, error } = useGithubBuildRepositories(organizationSlug, opened);
  const waiting = (repository: GithubBuildRepository) =>
    repository.readiness === "setup_needed" && opened.has(githubBuildRepositoryKey(repository));

  if (error && !repositories) {
    return (
      <Card size="sm">
        <CardHeader>
          <CardTitle>GitHub Actions</CardTitle>
          <CardDescription>Could not check the repositories on GitHub.</CardDescription>
        </CardHeader>
      </Card>
    );
  }
  if (!repositories?.length) return null;
  const ready = repositories.filter((repository) => repository.readiness === "ready").length;

  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>GitHub Actions</CardTitle>
        <CardDescription>
          {ready} of {repositories.length} repositories set up
        </CardDescription>
        <CardAction>
          <Dialog>
            <DialogTrigger render={<Button size="sm" variant="outline" />}>Manage</DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>GitHub Actions</DialogTitle>
                <DialogDescription>
                  Each repository needs one small workflow file. It only runs when Ployz starts a build, and build secrets are sent to the runner.
                </DialogDescription>
              </DialogHeader>
              <ItemGroup>
                {repositories.map((repository) => (
                  <Item key={githubBuildRepositoryKey(repository)} variant="outline" size="sm">
                    <ItemContent>
                      <ItemTitle>{repository.fullName}</ItemTitle>
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
            </DialogContent>
          </Dialog>
        </CardAction>
      </CardHeader>
    </Card>
  );
}
