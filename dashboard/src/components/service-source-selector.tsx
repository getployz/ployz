import { normalizePublicGithubRepository } from "#/modules/github/public-repository";
import { resolvePublicGithubRepositoryServerFn } from "#/modules/github/github.functions";
import type { ServiceGitAccess } from "@ployz/sdk";
import { GithubRepositoryRefreshNotice } from "./github-repository-refresh-notice";
import { Command as CommandPrimitive } from "cmdk";
import { SourcePickerInput, SourcePickerLayout } from "#/components/source-picker-layout";
import { type ReactNode, Suspense, useDeferredValue, useEffect, useState } from "react";
import { ChevronRightIcon, ExternalLinkIcon, InfoIcon, TriangleAlertIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { type QueryClient, useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { count, ilike, useLiveQuery } from "@tanstack/react-db";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { InputGroupAddon, InputGroupButton, InputGroupInput } from "#/components/ui/input-group";
import { Item, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "#/components/ui/item";
import { imageRegistryLink, isValidImageReference } from "#/components/image-registry-link";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { Command, CommandDialog, CommandGroup, CommandItem, CommandList, CommandSeparator, CommandShortcut } from "#/components/ui/command";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import { githubBranchesQueryOptions, githubKeys, githubRepoAccessQueryOptions } from "#/modules/github/github.queries";
import { getGithubReposCollection, preloadGithubRepos, useGithubReposReadState } from "#/modules/github/github.collection";
import { requestGithubRepoSyncServerFn } from "#/modules/github/github.functions";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { toErrorMessage } from "#/lib/error-message";
import { getGitRepoSelectorState } from "#/components/service-source-selector-state";

const imageExamples = [
  "hello-world",
  "ghcr.io/acme/api:latest",
  "quay.io/acme/api:latest",
  "registry.gitlab.com/acme/api:latest",
  "mcr.microsoft.com/dotnet/aspnet:10.0",
];
const INITIAL_GITHUB_REPO_LIMIT = 30;
const FILTERED_GITHUB_REPO_LIMIT = 200;

export type GitRepoSelection = {
  fullName: string;
  repositoryId: number;
  access: ServiceGitAccess;
  defaultBranch: string;
};

type GitRepoSelectorProps = {
  organizationSlug: string;
  query: string;
  disabled?: boolean;
  onSelectRepo: (repo: GitRepoSelection) => void | Promise<void>;
};

type ImageSelectorProps = {
  disabled?: boolean;
  onBack?: () => void;
  onSelectImage: (image: string) => void | Promise<void>;
};

type GitRepoSelectorDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  organizationSlug: string;
  onSelectRepo: (repo: GitRepoSelection) => void | Promise<void>;
};

type ImageSelectorDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelectImage: (image: string) => void | Promise<void>;
};

type GitBranchSelectorProps = {
  organizationSlug: string;
  repositoryId: number;
  installationId: number | null;
  query: string;
  defaultBranch?: string;
  disabled?: boolean;
  onSelectBranch: (branchName: string) => void | Promise<void>;
};

type GitBranchSelectorDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  organizationSlug: string;
  repositoryFullName: string;
  repositoryId: number;
  installationId: number | null;
  onSelectBranch: (branchName: string) => void | Promise<void>;
};

function SelectorEmpty({ children }: { children: ReactNode }) {
  return (
    <div className="py-6 text-center text-sm text-muted-foreground">
      {children}
    </div>
  );
}

function SelectorLoading() {
  return (
    <div role="status" aria-label="Loading options" className="flex flex-col gap-2 p-2">
      {[0, 1, 2].map((row) => <Skeleton key={row} className="h-8 w-full" />)}
    </div>
  );
}

type SelectorDialogState<T> = {
  query: string;
  setQuery: (value: string) => void;
  error: string | null;
  isPending: boolean;
  runSelect: (value: T) => Promise<void>;
};

function useSelectorDialogState<T>(input: {
  onOpenChange: (open: boolean) => void;
  onSelect: (value: T) => void | Promise<void>;
  errorFallback: string;
}): SelectorDialogState<T> {
  const [query, setQuery] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [isPending, setIsPending] = useState(false);

  return {
    query,
    setQuery,
    error,
    isPending,
    runSelect: async (value) => {
      setError(null);
      setIsPending(true);

      try {
        await input.onSelect(value);
        input.onOpenChange(false);
      } catch (nextError) {
        setError(toErrorMessage(nextError, input.errorFallback));
      } finally {
        setIsPending(false);
      }
    },
  };
}

type SelectorCommandDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: string;
  errorTitle: string;
  error: string | null;
  children: ReactNode;
};

function SelectorCommandDialog({
  open,
  onOpenChange,
  title,
  description,
  errorTitle,
  error,
  children,
}: SelectorCommandDialogProps) {
  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title={title}
      description={description}
      className="max-w-md"
      showCloseButton={false}
      surface="unstyled"
    >
      <div className="flex w-full min-w-0 flex-col gap-2">
        {error ? (
          <Alert variant="destructive">
            <AlertTitle>{errorTitle}</AlertTitle>
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        ) : null}
        {children}
      </div>
    </CommandDialog>
  );
}

const githubConnectHref = (organizationSlug: string) =>
  `/api/github/connect?${new URLSearchParams({ organization: organizationSlug }).toString()}`;

/** Cross-origin reads throw while the popup is on GitHub; before the first redirect it is about:blank. */
function returnedToPloyz(popup: Window) {
  try {
    const { protocol, origin, pathname } = popup.location;
    return protocol !== "about:" && origin === window.location.origin && !pathname.startsWith("/api/github/");
  } catch {
    return false;
  }
}

let onReturnFromGithub: (() => void) | undefined;

function openGithub(queryClient: QueryClient, organizationSlug: string) {
  const href = githubConnectHref(organizationSlug);
  const popup = window.open(href, "_blank", "width=1020,height=680");
  if (!popup) {
    window.location.assign(href);
    return;
  }
  // Returning from the popup fires focus but not visibilitychange, so recheck access here, and
  // resync in case GitHub's webhook never arrives. Keep resyncing on focus until one succeeds
  // after the flow ends: the popup closed, or GitHub sent it back to a Ployz page.
  if (onReturnFromGithub) window.removeEventListener("focus", onReturnFromGithub);
  const onReturn = () => {
    const done = popup.closed || returnedToPloyz(popup);
    void queryClient.invalidateQueries({ queryKey: githubKeys.access(organizationSlug) });
    void requestGithubRepoSyncServerFn({ data: { organizationSlug } }).then(() => {
      if (done && onReturnFromGithub === onReturn) {
        window.removeEventListener("focus", onReturn);
        onReturnFromGithub = undefined;
      }
    }, () => {});
  };
  onReturnFromGithub = onReturn;
  window.addEventListener("focus", onReturn);
}

function GitRepoSelectorConnect({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const { data: accessState } = useSuspenseQuery(githubRepoAccessQueryOptions(organizationSlug));
  if (!accessState.hasInstallations) {
    return (
      <Empty>
        <EmptyHeader>
          <EmptyMedia>
            <GitHubMarkIcon className="size-8 text-muted-foreground" />
          </EmptyMedia>
          <EmptyTitle>Connect GitHub</EmptyTitle>
          <EmptyDescription>
            Give Ployz access to the repositories you want to deploy.
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Button onClick={() => openGithub(queryClient, organizationSlug)}>Connect GitHub</Button>
        </EmptyContent>
      </Empty>
    );
  }
  return null;
}

/** Pinned below the CommandList so it stays visible while the repositories scroll. */
export function GitRepoSelectorFooter({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const { data: accessState } = useSuspenseQuery(githubRepoAccessQueryOptions(organizationSlug));
  if (!accessState.hasInstallations) return null;
  return (
    <>
      <CommandSeparator />
      <Item size="xs">
        <ItemContent>
          <ItemDescription>Missing a repo?</ItemDescription>
        </ItemContent>
        <ItemActions>
          <Button variant="outline" size="sm" onClick={() => openGithub(queryClient, organizationSlug)}>
            <GitHubMarkIcon data-icon="inline-start" />
            Add from GitHub
            <ExternalLinkIcon data-icon="inline-end" />
          </Button>
        </ItemActions>
      </Item>
    </>
  );
}

function groupByAccount<R extends { full_name: string }>(repos: ReadonlyArray<R>) {
  const groups = new Map<string, R[]>();
  for (const repo of repos) {
    const account = repo.full_name.split("/")[0] ?? repo.full_name;
    groups.set(account, [...(groups.get(account) ?? []), repo]);
  }
  return [...groups];
}

function GitRepoSelectorResults({
  organizationSlug,
  query,
  disabled = false,
  onSelectRepo,
}: GitRepoSelectorProps) {
  const { queryClient, sessionId } = useCollectionScope();
  const scope = { queryClient, sessionId, organizationSlug };
  const githubRepos = getGithubReposCollection(scope);
  // Query errors must repaint even when the collection retains identical rows.
  const { isError, dataUpdatedAt } = useGithubReposReadState(scope);
  const { isReady: reposReady } = useLiveQuery(githubRepos);
  const { data: accessState } = useSuspenseQuery(
    githubRepoAccessQueryOptions(organizationSlug)
  );
  const deferredQuery = useDeferredValue(query);
  const normalizedQuery = deferredQuery.trim();

  const { data: repoCountRows } =
    useLiveQuery({
      queryKey: ["github-repo-count", githubRepos.id],
      gcTime: 1,
      query: (q) => q.from({ repo: githubRepos }).select(({ repo }) => ({ count: count(repo.id) })),
    });

  const { data: repos = [], isLoading } = useLiveQuery({
    queryKey: ["github-repo-search", githubRepos.id, normalizedQuery],
    gcTime: 1,
    query: (q) => {
      const repoQuery = q
        .from({ repo: githubRepos })
        .orderBy(({ repo }) => repo.repo_updated_at, "desc");

      if (normalizedQuery) {
        return repoQuery
          .where(({ repo }) => ilike(repo.full_name, `%${normalizedQuery}%`))
          .limit(FILTERED_GITHUB_REPO_LIMIT);
      }

      return repoQuery.limit(INITIAL_GITHUB_REPO_LIMIT);
    },
  });
  const repoCount = repoCountRows?.[0]?.count ?? 0;
  const selectorState = getGitRepoSelectorState({
    hasInstallations: accessState.hasInstallations,
    repoCount,
    filteredRepoCount: repos.length,
  });

  if (isError && dataUpdatedAt === 0) return <GithubRepositoryRefreshNotice initial />;
  if (!reposReady || isLoading) return <SelectorLoading />;

  if (selectorState === "no-installations") {
    return null;
  }

  return (
    <>
      {isError && <GithubRepositoryRefreshNotice />}
      <CommandSeparator />

      {selectorState === "empty" ? (
        <SelectorEmpty>No repositories found</SelectorEmpty>
      ) : selectorState === "no-results" ? (
        <SelectorEmpty>No repositories match your search</SelectorEmpty>
      ) : (
        groupByAccount(repos).map(([account, accountRepos]) => (
        <CommandGroup key={account} heading={account}>
          {accountRepos.map((repo) => (
            <CommandItem
              key={repo.id}
              value={repo.full_name}
              keywords={["git", "github", "repository", "repo"]}
              disabled={disabled}
              onSelect={() => {
                void onSelectRepo({
                  fullName: repo.full_name,
                  repositoryId: repo.id,
                  access: { type: "github-installation", installationId: repo.installation_id },
                  defaultBranch: repo.default_branch,
                });
              }}
            >
              <GitHubMarkIcon />
              <span>{repo.full_name}</span>
              <CommandShortcut>
                <ChevronRightIcon />
              </CommandShortcut>
            </CommandItem>
          ))}
        </CommandGroup>
        ))
      )}

      {!normalizedQuery && repoCount > INITIAL_GITHUB_REPO_LIMIT ? (
        <div className="px-2 pb-2 text-xs text-muted-foreground">
          Showing {INITIAL_GITHUB_REPO_LIMIT} of {repoCount} repositories. Type
          to narrow the list.
        </div>
      ) : null}

      {normalizedQuery && repos.length === FILTERED_GITHUB_REPO_LIMIT ? (
        <div className="px-2 pb-2 text-xs text-muted-foreground">
          Showing the first {FILTERED_GITHUB_REPO_LIMIT} matches. Refine your
          search to narrow the list.
        </div>
      ) : null}
    </>
  );
}

export function GitRepoSelector(props: GitRepoSelectorProps) {
  const { queryClient, sessionId } = useCollectionScope();
  const { organizationSlug } = props;
  // The results below suspend on access before subscribing to repositories; start both together.
  useEffect(() => {
    preloadGithubRepos({ queryClient, sessionId, organizationSlug });
  }, [queryClient, sessionId, organizationSlug]);
  const publicName = normalizePublicGithubRepository(props.query);
  const connectPublic = useMutation({
    mutationFn: async () => {
      const repository = await resolvePublicGithubRepositoryServerFn({ data: { repository: props.query } });
      await props.onSelectRepo(repository);
    },
  });
  return (
    <>
      {publicName ? <CommandGroup heading="Public repository">
        <CommandItem disabled={props.disabled || connectPublic.isPending} onSelect={() => connectPublic.mutate()}>
          <GitHubMarkIcon /> {connectPublic.isPending ? "Connecting…" : `Deploy ${publicName}`}
        </CommandItem>
      </CommandGroup> : props.query.includes("/") ? <SelectorEmpty>Enter owner/repo or a GitHub repository URL. Choose the branch separately.</SelectorEmpty> : null}
      {connectPublic.isError ? <Alert variant="destructive"><AlertDescription>Could not read this public repository. Check its address and visibility, then try again.</AlertDescription></Alert> : null}
      <Suspense fallback={<SelectorLoading />}>
        <GitRepoSelectorResults {...props} />
      </Suspense>
      <Suspense>
        <GitRepoSelectorConnect organizationSlug={organizationSlug} />
      </Suspense>
    </>
  );
}

function GitBranchSelectorResults({
  organizationSlug,
  repositoryId,
  installationId,
  query,
  defaultBranch,
  disabled = false,
  onSelectBranch,
}: GitBranchSelectorProps) {
  const { data: branchesData } = useSuspenseQuery(
    githubBranchesQueryOptions({
      organizationSlug,
      repositoryId,
      installationId,
    })
  );
  const deferredQuery = useDeferredValue(query);
  const normalizedQuery = deferredQuery.trim().toLowerCase();
  const filteredBranches = normalizedQuery
    ? branchesData.branches.filter((branch) =>
        branch.name.toLowerCase().includes(normalizedQuery)
      )
    : branchesData.branches;

  if (branchesData.branches.length === 0) {
    return <SelectorEmpty>No branches found</SelectorEmpty>;
  }

  if (filteredBranches.length === 0) {
    return <SelectorEmpty>No branches match your search</SelectorEmpty>;
  }

  return (
    <CommandGroup>
      {filteredBranches.map((branch) => (
        <CommandItem
          key={branch.name}
          value={branch.name}
          keywords={["branch", "git", "github"]}
          disabled={disabled}
          onSelect={() => onSelectBranch(branch.name)}
        >
          <span>{branch.name}</span>
          {branch.name === defaultBranch ? (
            <CommandShortcut>Default</CommandShortcut>
          ) : null}
        </CommandItem>
      ))}
    </CommandGroup>
  );
}

export function GitBranchSelector(props: GitBranchSelectorProps) {
  return (
    <Suspense fallback={<SelectorLoading />}>
      <GitBranchSelectorResults {...props} />
    </Suspense>
  );
}

export function GitBranchSelectorDialog({
  open,
  onOpenChange,
  organizationSlug,
  repositoryFullName,
  repositoryId,
  installationId,
  onSelectBranch,
}: GitBranchSelectorDialogProps) {
  if (!open) {
    return null;
  }

  return (
    <OpenGitBranchSelectorDialog
      open={open}
      onOpenChange={onOpenChange}
      organizationSlug={organizationSlug}
      repositoryFullName={repositoryFullName}
      repositoryId={repositoryId}
      installationId={installationId}
      onSelectBranch={onSelectBranch}
    />
  );
}

function OpenGitBranchSelectorDialog({
  open,
  onOpenChange,
  organizationSlug,
  repositoryFullName,
  repositoryId,
  installationId,
  onSelectBranch,
}: GitBranchSelectorDialogProps) {
  const dialog = useSelectorDialogState<string>({
    onOpenChange,
    onSelect: onSelectBranch,
    errorFallback: "Refresh the repository data and try again.",
  });

  return (
    <SelectorCommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Select branch"
      description={`Choose a branch in ${repositoryFullName}`}
      errorTitle="Couldn’t select branch"
      error={dialog.error}
    >
      <SourcePickerLayout title="GitHub Branch">
      <Command shouldFilter={false} className="gap-3 p-0">
        <SourcePickerInput onBack={() => onOpenChange(false)} disabled={dialog.isPending}>
          <CommandPrimitive.Input asChild value={dialog.query} onValueChange={dialog.setQuery}>
            <InputGroupInput autoFocus aria-label="Search branches" placeholder="Search branches…" disabled={dialog.isPending} />
          </CommandPrimitive.Input>
        </SourcePickerInput>
        <CommandList>
          <GitBranchSelector
            organizationSlug={organizationSlug}
            repositoryId={repositoryId}
            installationId={installationId}
            query={dialog.query}
            disabled={dialog.isPending}
            onSelectBranch={dialog.runSelect}
          />
        </CommandList>
      </Command>
      </SourcePickerLayout>
    </SelectorCommandDialog>
  );
}

export function ImageSelector({
  disabled = false,
  onBack,
  onSelectImage,
}: ImageSelectorProps) {
  const [value, setValue] = useState("");
  const trimmedValue = value.trim();
  const validImage = isValidImageReference(trimmedValue);
  const invalidImage = trimmedValue.length > 0 && !validImage;
  const registryLink = imageRegistryLink(trimmedValue);

  return (
    <SourcePickerLayout title="Docker image">
      <form
        className="flex min-w-0 flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault();
          if (validImage && !disabled) void onSelectImage(trimmedValue);
        }}
      >
        <SourcePickerInput onBack={onBack} disabled={disabled}>
          <InputGroupInput
            aria-label="Container image"
            aria-invalid={invalidImage || undefined}
            aria-describedby={invalidImage ? "invalid-docker-image" : undefined}
            autoFocus
            autoComplete="off"
            spellCheck={false}
            disabled={disabled}
            value={value}
            onChange={(event) => setValue(event.target.value)}
            placeholder="nginx:latest"
          />
          <InputGroupAddon align="inline-end">
            <InputGroupButton type="submit" disabled={disabled || !validImage}>
              {disabled ? <Spinner data-icon="inline-start" /> : null}
              Continue
            </InputGroupButton>
          </InputGroupAddon>
        </SourcePickerInput>
        {invalidImage ? (
          <Item state="warning" role="status" id="invalid-docker-image">
            <ItemMedia variant="icon"><TriangleAlertIcon /></ItemMedia>
            <ItemContent>Invalid Docker image</ItemContent>
          </Item>
        ) : trimmedValue ? (
          <Item variant="muted">
            <ItemContent>
              {registryLink ? (
                <a href={registryLink} target="_blank" rel="noopener noreferrer" className="break-all underline underline-offset-4">
                  {registryLink.replace(/^https:\/\//, "")}
                </a>
              ) : <span>Enter a Docker image reference.</span>}
            </ItemContent>
          </Item>
        ) : (
          <>
            <Item state="info">
              <ItemContent>Enter a Docker image to deploy.</ItemContent>
              <ItemMedia variant="icon"><InfoIcon /></ItemMedia>
            </Item>
            <Item variant="muted">
              <ItemContent>
                <ItemTitle>Examples</ItemTitle>
                <ul className="list-disc space-y-1 pl-5">
                  {imageExamples.map((example) => <li key={example} className="break-all">{example}</li>)}
                </ul>
              </ItemContent>
            </Item>
          </>
        )}
      </form>
    </SourcePickerLayout>
  );
}

export function GitRepoSelectorDialog({
  open,
  onOpenChange,
  organizationSlug,
  onSelectRepo,
}: GitRepoSelectorDialogProps) {
  if (!open) {
    return null;
  }

  return (
    <OpenGitRepoSelectorDialog
      open={open}
      onOpenChange={onOpenChange}
      organizationSlug={organizationSlug}
      onSelectRepo={onSelectRepo}
    />
  );
}

function OpenGitRepoSelectorDialog({
  open,
  onOpenChange,
  organizationSlug,
  onSelectRepo,
}: GitRepoSelectorDialogProps) {
  const dialog = useSelectorDialogState<GitRepoSelection>({
    onOpenChange,
    onSelect: onSelectRepo,
    errorFallback: "Check the GitHub App’s repository access and try again.",
  });

  return (
    <SelectorCommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Choose repository"
      description="Choose a GitHub repository"
      errorTitle="Couldn’t connect repository"
      error={dialog.error}
    >
      <SourcePickerLayout title="GitHub repository">
      <Command shouldFilter={false} className="gap-3 p-0">
        <SourcePickerInput onBack={() => onOpenChange(false)} disabled={dialog.isPending}>
            <CommandPrimitive.Input
              asChild
              value={dialog.query}
              onValueChange={dialog.setQuery}
            >
              <InputGroupInput autoFocus aria-label="Search GitHub repositories" placeholder="Search repositories or paste a GitHub URL…" disabled={dialog.isPending} />
            </CommandPrimitive.Input>
        </SourcePickerInput>
        <CommandList>
          <GitRepoSelector
            organizationSlug={organizationSlug}
            query={dialog.query}
            disabled={dialog.isPending}
            onSelectRepo={dialog.runSelect}
          />
        </CommandList>
        <Suspense>
          <GitRepoSelectorFooter organizationSlug={organizationSlug} />
        </Suspense>
      </Command>
      </SourcePickerLayout>
    </SelectorCommandDialog>
  );
}

export function ImageSelectorDialog({
  open,
  onOpenChange,
  onSelectImage,
}: ImageSelectorDialogProps) {
  if (!open) {
    return null;
  }

  return (
    <OpenImageSelectorDialog
      open={open}
      onOpenChange={onOpenChange}
      onSelectImage={onSelectImage}
    />
  );
}

function OpenImageSelectorDialog({
  open,
  onOpenChange,
  onSelectImage,
}: ImageSelectorDialogProps) {
  const dialog = useSelectorDialogState<string>({
    onOpenChange,
    onSelect: onSelectImage,
    errorFallback: "Check the image name and try again.",
  });

  return (
    <SelectorCommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Choose container image"
      description="Choose a container image"
      errorTitle="Couldn’t select image"
      error={dialog.error}
    >
      <ImageSelector
        disabled={dialog.isPending}
        onBack={() => onOpenChange(false)}
        onSelectImage={dialog.runSelect}
      />
    </SelectorCommandDialog>
  );
}
