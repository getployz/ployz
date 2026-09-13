import { GithubRepositoryRefreshNotice } from "./github-repository-refresh-notice";
import { useLoaderData } from "@tanstack/react-router";
import {
  type ReactNode,
  Suspense,
  useDeferredValue,
  useState,
} from "react";
import {
  ChevronRightIcon,
  PackageIcon,
  RefreshCwIcon,
  Settings2Icon,
} from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import {
  useMutation,
  useQuery,
  useQueryClient,
  useSuspenseQuery,
  skipToken,
} from "@tanstack/react-query";
import { count, ilike, useLiveQuery } from "@tanstack/react-db";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import {
  Command,
  CommandDialog,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
  CommandShortcut,
} from "#/components/ui/command";
import { Spinner } from "#/components/ui/spinner";
import {
  githubBranchesQueryOptions,
  githubInstallUrlQueryOptions,
  githubRepoAccessQueryOptions,
  githubKeys,
} from "#/modules/github/github.queries";
import { getGithubReposCollection, getRawGithubReposCollection, githubReposQueryKey } from "#/modules/github/github.collection";
import { requestGithubRepoSyncServerFn } from "#/modules/github/github.functions";
import { toErrorMessage } from "#/lib/error-message";
import { getGitRepoSelectorState } from "#/components/service-source-selector-state";

const imageExamples = [
  "ghcr.io/acme/api:latest",
  "registry.example.com/acme/api:main",
  "docker.io/library/nginx:stable",
];
const INITIAL_GITHUB_REPO_LIMIT = 30;
const FILTERED_GITHUB_REPO_LIMIT = 200;

export type GitRepoSelection = {
  fullName: string;
  repositoryId: number;
  installationId: number;
  defaultBranch: string;
};

type GitRepoSelectorProps = {
  query: string;
  disabled?: boolean;
  onSelectRepo: (repo: GitRepoSelection) => void | Promise<void>;
};

type ImageSelectorProps = {
  disabled?: boolean;
  onSelectImage: (image: string) => void | Promise<void>;
};

type GitRepoSelectorDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelectRepo: (repo: GitRepoSelection) => void | Promise<void>;
};

type ImageSelectorDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelectImage: (image: string) => void | Promise<void>;
};

type GitBranchSelectorProps = {
  repositoryFullName: string;
  repositoryId: number;
  installationId: number;
  query: string;
  disabled?: boolean;
  onSelectBranch: (branchName: string) => void | Promise<void>;
};

type GitBranchSelectorDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  repositoryFullName: string;
  repositoryId: number;
  installationId: number;
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
    <div className="flex items-center justify-center py-6">
      <Spinner className="text-muted-foreground" />
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

function GitRepoSelectorActions() {
  const queryClient = useQueryClient();
  const { data: installUrlData } = useSuspenseQuery(
    githubInstallUrlQueryOptions()
  );
  const { mutateAsync: requestRepoSync, isPending: isRefreshing } = useMutation(
    {
      mutationKey: [...githubKeys.repos(), "refresh"],
      mutationFn: () => requestGithubRepoSyncServerFn(),
      onSuccess: async () => {
        await queryClient.invalidateQueries({ queryKey: githubKeys.access() });
      },
    }
  );

  return (
    <CommandGroup>
      <CommandItem
        value="Configure GitHub App"
        keywords={["github", "configure", "app"]}
        onSelect={() => {
          if (installUrlData.url) {
            window.open(installUrlData.url, "_blank", "width=1020,height=680");
          }
        }}
      >
        <Settings2Icon />
        <span>Set up GitHub</span>
        <CommandShortcut>
          <Button
            variant="outline"
            size="sm"
            onClick={async (event) => {
              event.preventDefault();
              event.stopPropagation();
              await requestRepoSync();
            }}
          >
            {isRefreshing ? (
              <Spinner data-icon="inline-start" />
            ) : (
              <RefreshCwIcon data-icon="inline-start" />
            )}
            {isRefreshing ? "Refreshing" : "Refresh"}
          </Button>
        </CommandShortcut>
      </CommandItem>
    </CommandGroup>
  );
}

function GitRepoSelectorResults({
  query,
  disabled = false,
  onSelectRepo,
}: GitRepoSelectorProps) {
  const queryClient = useQueryClient();
  const { session } = useLoaderData({ from: "__root__" });
  if (!session) throw new Error("Authentication is required.");
  const scope = { queryClient, userId: session.user.id, sessionId: session.session.id };
  const raw = getRawGithubReposCollection(scope);
  // Query errors must repaint even when the collection retains identical rows.
  const { isError, dataUpdatedAt } = useQuery({
    queryKey: githubReposQueryKey(scope), queryFn: skipToken, gcTime: 1,
  });
  const { isReady: rawReady } = useLiveQuery(raw);
  const githubRepos = rawReady ? getGithubReposCollection(scope) : undefined;
  const { data: accessState } = useSuspenseQuery(
    githubRepoAccessQueryOptions()
  );
  const deferredQuery = useDeferredValue(query);
  const normalizedQuery = deferredQuery.trim();

  const { data: repoCountRows } =
    useLiveQuery((q) => githubRepos
      ? { gcTime: 1, query: q.from({ repo: githubRepos }).select(({ repo }) => ({ count: count(repo.id) })) }
      : undefined, [githubRepos]);

  const { data: repos = [], isLoading } = useLiveQuery(
    (q) => {
      if (!githubRepos) return undefined;
      const repoQuery = q
        .from({ repo: githubRepos })
        .orderBy(({ repo }) => repo.repo_updated_at, "desc");

      if (normalizedQuery) {
        return { gcTime: 1, query: repoQuery
          .where(({ repo }) => ilike(repo.full_name, `%${normalizedQuery}%`))
          .limit(FILTERED_GITHUB_REPO_LIMIT) };
      }

      return { gcTime: 1, query: repoQuery.limit(INITIAL_GITHUB_REPO_LIMIT) };
    },
    [githubRepos, normalizedQuery],
  );
  const repoCount = repoCountRows?.[0]?.count ?? 0;
  const selectorState = getGitRepoSelectorState({
    configured: accessState.configured,
    hasInstallations: accessState.hasInstallations,
    repoCount,
    filteredRepoCount: repos.length,
  });

  if (isError && dataUpdatedAt === 0) return <GithubRepositoryRefreshNotice initial />;
  if (!rawReady || isLoading) return <SelectorEmpty><Spinner /></SelectorEmpty>;

  if (selectorState === "not-configured") {
    return (
      <SelectorEmpty>
        <p>GitHub isn’t set up.</p>
        <p className="mt-1">
          Add `GITHUB_APP_PRIVATE_KEY` and `GITHUB_APP_SLUG` to load
          repositories.
        </p>
      </SelectorEmpty>
    );
  }

  if (selectorState === "no-installations") {
    return <SelectorEmpty>No GitHub installations found</SelectorEmpty>;
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
        <CommandGroup>
          {repos.map((repo) => (
            <CommandItem
              key={repo.id}
              value={repo.full_name}
              keywords={["git", "github", "repository", "repo"]}
              disabled={disabled}
              onSelect={() => {
                void onSelectRepo({
                  fullName: repo.full_name,
                  repositoryId: repo.id,
                  installationId: repo.installation_id,
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
  return (
    <>
      <Suspense>
        <GitRepoSelectorActions />
      </Suspense>
      <Suspense fallback={<SelectorLoading />}>
        <GitRepoSelectorResults {...props} />
      </Suspense>
    </>
  );
}

function GitBranchSelectorResults({
  repositoryFullName,
  repositoryId,
  installationId,
  query,
  disabled = false,
  onSelectBranch,
}: GitBranchSelectorProps) {
  const { data: branchesData } = useSuspenseQuery(
    githubBranchesQueryOptions({
      repositoryFullName,
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

  if (!branchesData.configured) {
    return <SelectorEmpty>GitHub isn’t set up</SelectorEmpty>;
  }

  if (!branchesData.hasInstallations) {
    return <SelectorEmpty>No GitHub installations found</SelectorEmpty>;
  }

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
        </CommandItem>
      ))}
    </CommandGroup>
  );
}

export function GitBranchSelectorDialog({
  open,
  onOpenChange,
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
      description="Choose a GitHub branch"
      errorTitle="Couldn’t select branch"
      error={dialog.error}
    >
      <Command shouldFilter={false}>
        <CommandInput
          aria-label="Search branches"
          disabled={dialog.isPending}
          value={dialog.query}
          onValueChange={dialog.setQuery}
          placeholder="Search branches…"
        />
        <CommandList>
          <Suspense fallback={<SelectorLoading />}>
            <GitBranchSelectorResults
              repositoryFullName={repositoryFullName}
              repositoryId={repositoryId}
              installationId={installationId}
              query={dialog.query}
              disabled={dialog.isPending}
              onSelectBranch={dialog.runSelect}
            />
          </Suspense>
        </CommandList>
      </Command>
    </SelectorCommandDialog>
  );
}

export function ImageSelector({
  disabled = false,
  onSelectImage,
}: ImageSelectorProps) {
  const [value, setValue] = useState("");
  const trimmedValue = value.trim();

  return (
    <>
      <CommandGroup>
        <CommandItem value="image-input" forceMount>
          <input
            type="text"
            aria-label="Container image"
            value={value}
            onChange={(event) => setValue(event.target.value)}
            placeholder="nginx:latest"
            className="w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground"
            onKeyDown={(event) => {
              if (event.key === "Enter" && trimmedValue && !disabled) {
                event.preventDefault();
                void onSelectImage(trimmedValue);
              }
            }}
          />
        </CommandItem>
      </CommandGroup>

      {trimmedValue ? (
        <>
          <CommandSeparator />
          <CommandGroup>
            <CommandItem
              value={`Use ${trimmedValue}`}
              disabled={disabled}
              onSelect={() => {
                void onSelectImage(trimmedValue);
              }}
            >
              <PackageIcon />
              <span className="truncate">{trimmedValue}</span>
            </CommandItem>
          </CommandGroup>
        </>
      ) : null}

      <CommandSeparator />

      <CommandGroup heading="Examples">
        {imageExamples.map((example) => (
          <CommandItem
            key={example}
            value={example}
            keywords={["image", "container", "registry"]}
            onSelect={() => setValue(example)}
          >
            <PackageIcon />
            <span className="font-mono text-xs">{example}</span>
          </CommandItem>
        ))}
      </CommandGroup>
    </>
  );
}

export function GitRepoSelectorDialog({
  open,
  onOpenChange,
  onSelectRepo,
}: GitRepoSelectorDialogProps) {
  if (!open) {
    return null;
  }

  return (
    <OpenGitRepoSelectorDialog
      open={open}
      onOpenChange={onOpenChange}
      onSelectRepo={onSelectRepo}
    />
  );
}

function OpenGitRepoSelectorDialog({
  open,
  onOpenChange,
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
      <Command shouldFilter={false}>
        <CommandInput
          aria-label="Search GitHub repositories"
          disabled={dialog.isPending}
          value={dialog.query}
          onValueChange={dialog.setQuery}
          placeholder="Search GitHub repositories…"
        />
        <CommandList>
          <GitRepoSelector
            query={dialog.query}
            disabled={dialog.isPending}
            onSelectRepo={dialog.runSelect}
          />
        </CommandList>
      </Command>
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
      <Command>
        <CommandList>
          <ImageSelector
            disabled={dialog.isPending}
            onSelectImage={dialog.runSelect}
          />
        </CommandList>
      </Command>
    </SelectorCommandDialog>
  );
}
