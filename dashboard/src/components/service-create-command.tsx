import { useCollectionScope } from "#/collections/use-collection-scope";
import { useStillHere } from "#/hooks/use-still-here";
import { preloadGithubRepos } from "#/modules/github/github.collection";
import { useEffect, useRef, useState } from "react";
import { Command as CommandPrimitive } from "cmdk";
import { ChevronRightIcon } from "lucide-react";
import { useNavigate } from "@tanstack/react-router";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { InputGroupInput } from "#/components/ui/input-group";
import { SourcePickerInput, SourcePickerLayout } from "#/components/source-picker-layout";
import {
  Command,
  CommandGroup,
  CommandItem,
  CommandList,
  CommandShortcut,
} from "#/components/ui/command";
import {
  GitRepoSelector,
  ImageSelector,
} from "#/components/service-source-selector";
import {
  type CreateMenuItem,
  type CreatePanel,
  getCreateMenuItems,
} from "#/components/create-menu-items";
import { DATABASE_LOGOS } from "#/components/icons/database-logos";
import { Spinner } from "#/components/ui/spinner";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_SERVICE_ROUTE_TO,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import {
  type NewServicePlacement,
  useCreateStoreDatabase,
  useCreateStoreService,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/useServiceCreator";
import { DATABASE_PRESETS, type DatabasePreset } from "#/modules/config-store/database-presets";
import { randomName, type NewServiceSource } from "#/modules/config-store/store-services";
import { environmentsQuery, fetchStoreView } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import type { EnvironmentId, ProjectId } from "@ployz/sdk";

type InitialPanel = CreatePanel;
type Panel = { kind: InitialPanel };
type CreateMode = "project" | "service";

function pickerPresentation(panel: Panel, mode: CreateMode) {
  if (panel.kind === "git") {
    return {
      title: "GitHub Repository",
      ariaLabel: "Search GitHub repositories",
      placeholder: "Search repositories or paste a GitHub URL…",
    };
  }
  if (panel.kind === "database") {
    return {
      title: "Database",
      ariaLabel: "Choose a database",
      placeholder: "Choose a database…",
    };
  }
  return {
    title: mode === "project" ? "Add your app" : "Add service",
    ariaLabel: "Choose a source",
    placeholder: "Choose a source…",
  };
}

type ProjectCommandProps = {
  mode?: "project";
  organizationSlug: string;
  initialPanel?: InitialPanel;
};

type ServiceCommandProps = {
  mode: "service";
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  canvasPosition: {
    x: number;
    y: number;
  };
  initialPanel?: InitialPanel;
  /** `stillHere` is false once the user moved on: close up, but don't take them anywhere. */
  onCreated?: (result: { service: { id: string } }, stillHere: boolean) => void | Promise<void>;
  onCreateVolume?: () => void;
  /** Says when a create starts and ends, so the dialog around it can stay open meanwhile. */
  onPendingChange?: (pending: boolean) => void;
};

type ServiceCreateCommandProps = ProjectCommandProps | ServiceCommandProps;

type RootPanelProps = {
  mode: CreateMode;
  onSelectItem: (item: CreateMenuItem) => void;
  isPending: boolean;
};

type GitPanelReposProps = {
  mode: CreateMode;
  query: string;
  disabled: boolean;
  onSelectRepo: (repo: {
    fullName: string;
    repositoryId: number;
    access: import("@ployz/sdk").ServiceGitAccess;
    defaultBranch: string;
  }) => void;
};

type GitPanelProps = GitPanelReposProps;

function RootPanel({ mode, onSelectItem, isPending }: RootPanelProps) {
  const { queryClient, sessionId, userId } = useCollectionScope();
  const [chosen, setChosen] = useState<CreateMenuItem["id"] | null>(null);
  // "GitHub repository" is one tap away: its picker opens with repositories already read.
  useEffect(() => {
    preloadGithubRepos({ queryClient, sessionId, userId });
  }, [queryClient, sessionId, userId]);
  const items = getCreateMenuItems({ includeEmptyProject: mode === "project" }).filter(({ id, panel }) =>
    mode === "service" || panel || id === "empty-project",
  );
  return (
    <CommandGroup>
      {items.map(
        (item) => (
          <CommandItem
            key={item.id}
            value={item.label}
            keywords={["create", mode]}
            disabled={isPending}
            onSelect={() => {
              setChosen(item.id);
              onSelectItem(item);
            }}
          >
            <item.icon />
            <span>{item.label}</span>
            {item.panel ? (
              <CommandShortcut>
                <ChevronRightIcon />
              </CommandShortcut>
            ) : isPending && item.id === chosen ? (
              <CommandShortcut>
                <Spinner />
              </CommandShortcut>
            ) : null}
          </CommandItem>
        ),
      )}
    </CommandGroup>
  );
}

function GitPanel(props: GitPanelProps) {
  return <GitRepoSelector {...props} />;
}

function DatabasePanel({ disabled, onSelectPreset }: { disabled: boolean; onSelectPreset: (preset: DatabasePreset) => void }) {
  return (
    <CommandGroup>
      {DATABASE_PRESETS.map((preset) => {
        const Logo = DATABASE_LOGOS[preset.id];
        return (
          <CommandItem key={preset.id} value={preset.label} disabled={disabled} onSelect={() => onSelectPreset(preset)}>
            <Logo />
            <span>{preset.label}</span>
          </CommandItem>
        );
      })}
    </CommandGroup>
  );
}

type CreationTarget = {
  projectSlug: string;
  environmentSlug: string;
  environmentId: string;
  canvasPosition: { x: number; y: number };
};

function useServiceCreateActions({
  props,
  setPanel,
  setQuery,
}: {
  props: ServiceCreateCommandProps;
  setPanel: (panel: Panel) => void;
  setQuery: (query: string) => void;
}) {
  const collectionScope = useCollectionScope();
  const navigate = useNavigate();
  const markHere = useStillHere();
  const createStoreService = useCreateStoreService(props.organizationSlug);
  const createStoreDatabase = useCreateStoreDatabase(props.organizationSlug);
  // Holds the project a failed command already created, so any retry, from any panel, reuses it.
  const createdProjectRef = useRef<CreationTarget | null>(null);
  const storeWriter = useStoreWriter(props.organizationSlug);
  // The ref guards re-entry (two clicks in one tick); the state only renders it.
  const creatingRef = useRef(false);
  const [creating, setCreating] = useState(false);
  const [failure, setFailure] = useState<unknown>(null);

  function resetPanelState() {
    setQuery("");
    setFailure(null);
  }

  function setActivePanel(nextPanel: InitialPanel) {
    resetPanelState();
    setPanel({ kind: nextPanel });
  }

  /** Runs one command from click until navigation lands; its failure shows in the alert. */
  async function whileCreating(action: () => Promise<void>) {
    if (creatingRef.current) return;
    creatingRef.current = true;
    setCreating(true);
    if (props.mode === "service") props.onPendingChange?.(true);
    setFailure(null);
    try {
      await action();
      createdProjectRef.current = null;
    } catch (error) {
      setFailure(error);
    } finally {
      creatingRef.current = false;
      setCreating(false);
      if (props.mode === "service") props.onPendingChange?.(false);
    }
  }

  async function serviceModeTarget(props: ServiceCommandProps): Promise<CreationTarget> {
    const listed = await fetchStoreView(props.organizationSlug, collectionScope, environmentsQuery(props.projectSlug));
    const environment = listed.environments.find((row) => row.name === props.environmentSlug);
    if (!environment) throw new Error("Environment not found");
    return { projectSlug: props.projectSlug, environmentSlug: environment.name, environmentId: environment.id,
      canvasPosition: props.canvasPosition };
  }

  /** A new Project, named like `brave-otter`, whose Default Environment takes the first Service. */
  async function createProjectTarget(): Promise<CreationTarget> {
    const made = createdProjectRef.current ?? await (async (): Promise<CreationTarget> => {
      // SAFETY: Project and Environment ids are UUIDs the caller mints; the Store checks them.
      const id = crypto.randomUUID() as ProjectId;
      // SAFETY: as above.
      const environment = crypto.randomUUID() as EnvironmentId;
      const written = await storeWriter.commit({ command: "create_project", id, name: randomName(), default_environment: environment })
        .isPersisted.promise;
      if (written.written !== "project") throw new Error("The Store didn't create the project.");
      return { projectSlug: written.project.name, environmentSlug: written.environment.name,
        environmentId: written.environment.id, canvasPosition: { x: 0, y: 0 } };
    })();
    createdProjectRef.current = made;
    return made;
  }

  async function navigateToEnvironment(target: CreationTarget) {
    await navigate({
      to: ENVIRONMENT_INDEX_ROUTE_TO,
      params: {
        organizationSlug: props.organizationSlug,
        projectSlug: target.projectSlug,
        environmentSlug: target.environmentSlug,
      },
      search: (prev) => prev,
    });
  }

  const createInTarget = (create: (placement: NewServicePlacement) => ReturnType<typeof createStoreService>) =>
    whileCreating(async () => {
      const stillHere = markHere();
      const target = props.mode === "service" ? await serviceModeTarget(props) : await createProjectTarget();
      const created = create({
        store: { project: target.projectSlug, environment: target.environmentSlug },
        environmentId: target.environmentId,
        position: target.canvasPosition,
      });
      if (props.mode === "service") {
        // Someone who navigated away while it saved stays there; the service still lands on the canvas.
        await props.onCreated?.(created, stillHere());
        return;
      }
      // A new Project's canvas has nothing cached to show the Service in before the Store has it.
      await created.persisted;
      await navigate({
        to: ENVIRONMENT_SERVICE_ROUTE_TO,
        params: { organizationSlug: props.organizationSlug, projectSlug: target.projectSlug, environmentSlug: target.environmentSlug,
          serviceId: created.service.id },
        search: (prev) => prev,
      });
    });

  const createServiceFromSource = (source: NewServiceSource) =>
    createInTarget((placement) => createStoreService(placement, source));
  const createDatabase = (preset: DatabasePreset) =>
    createInTarget((placement) => createStoreDatabase(placement, preset));

  function selectCreateItem({ id, panel }: CreateMenuItem) {
    if (panel) {
      setActivePanel(panel);
      return;
    }
    if (id === "empty-service") {
      void createServiceFromSource({ type: "empty" });
      return;
    }
    if (id === "volume") {
      // Only the canvas offers a Volume, and it places one itself.
      if (props.mode === "service") props.onCreateVolume?.();
      return;
    }

    void whileCreating(() => createProjectTarget().then(navigateToEnvironment));
  }

  return {
    error: failure,
    isPending: creating,
    selectCreateItem,
    setActivePanel,
    createServiceFromSource,
    createDatabase,
  };
}

export function ServiceCreateCommand(props: ServiceCreateCommandProps) {
  const mode: CreateMode = props.mode === "service" ? "service" : "project";
  const [panel, setPanel] = useState<Panel>({ kind: props.initialPanel ?? "root" });
  const [query, setQuery] = useState("");
  const presentation = pickerPresentation(panel, mode);
  const {
    error,
    isPending,
    selectCreateItem,
    setActivePanel,
    createServiceFromSource,
    createDatabase,
  } = useServiceCreateActions({ props, setPanel, setQuery });

  return (
    <div
      className="flex w-full min-w-0 flex-col gap-2"
      onKeyDownCapture={(event) => {
        if (panel.kind === "root" || event.key !== "Escape") {
          return;
        }

        event.preventDefault();
        event.stopPropagation();
        setActivePanel("root");
      }}
    >
      {error ? (
        <Alert variant="destructive">
          <AlertTitle>
            {mode === "service"
              ? "Couldn’t create service"
              : "Couldn’t create project"}
          </AlertTitle>
          <AlertDescription>
            {error instanceof Error
              ? error.message
              : mode === "service"
                ? "Check the selected source and try again."
                : "Try again."}
          </AlertDescription>
        </Alert>
      ) : null}

      {panel.kind === "image" ? (
        <ImageSelector
          disabled={isPending}
          onBack={() => setActivePanel("root")}
          onSelectImage={(image) => {
            void createServiceFromSource({ type: "image", image });
          }}
        />
      ) : (
      <SourcePickerLayout title={presentation.title}>
      <Command key={panel.kind} shouldFilter={panel.kind === "root" || panel.kind === "database"}>
        <SourcePickerInput
            onBack={
              panel.kind !== "root"
                ? () => setActivePanel("root")
                : undefined
            }
            disabled={isPending}
          >
            <CommandPrimitive.Input asChild value={query} onValueChange={setQuery}>
              <InputGroupInput
                autoFocus
                aria-label={presentation.ariaLabel}
                placeholder={presentation.placeholder}
                disabled={isPending}
              />
            </CommandPrimitive.Input>
        </SourcePickerInput>
        <CommandList>
          {panel.kind === "root" ? (
            <RootPanel
              mode={mode}
              onSelectItem={selectCreateItem}
              isPending={isPending}
            />
          ) : null}
          {panel.kind === "git" ? (
            <GitPanel
              mode={mode}
              query={query}
              disabled={isPending}
              onSelectRepo={({ fullName, defaultBranch }) => {
                void createServiceFromSource({ type: "git", repository: fullName, branch: defaultBranch });
              }}
            />
          ) : null}
          {panel.kind === "database" ? (
            <DatabasePanel disabled={isPending} onSelectPreset={(preset) => void createDatabase(preset)} />
          ) : null}
        </CommandList>
      </Command>
      </SourcePickerLayout>
      )}
    </div>
  );
}
