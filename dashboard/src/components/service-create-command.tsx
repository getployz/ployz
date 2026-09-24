import { applyCreatedService, applyCreatedResource } from "#/modules/environment-design/apply-created-node";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useState } from "react";
import { Command as CommandPrimitive } from "cmdk";
import { ChevronRightIcon } from "lucide-react";
import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
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
  type CreateMenuItemId,
  getCreateMenuItems,
} from "#/components/create-menu-items";
import { Spinner } from "#/components/ui/spinner";
import {
  createVariableGroupResourceServerFn,
  createVolumeResourceServerFn,
} from "#/modules/environment-design/resource-functions";
import {
  loadWorkspaceEnvironment,
} from "#/modules/environment-design/workspace.queries";
import { createEmptyProjectServerFn } from "#/modules/environment-design/workspace-functions";
import {
  createEmptyServiceSource,
  createGitServiceSource,
  createImageServiceSource,
  type ServiceSource,
} from "#/modules/environment-design/services";
import {
  getEnvironmentsCollection, getEnvironmentSummariesCollection, environmentSummary,
  getProjectsCollection,
} from "#/collections/collections";
import { createServiceServerFn } from "#/modules/environment-design/service-functions";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_RESOURCE_ROUTE_TO,
  ENVIRONMENT_SERVICE_ROUTE_TO,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";

type InitialPanel = "root" | "git" | "image";
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
  return {
    title: mode === "project" ? "Add your app" : "Add service",
    ariaLabel: "Choose a source",
    placeholder: "Choose a source…",
  };
}

type ProjectCreatedResult = {
  project: {
    slug: string;
  };
  environment: {
    id: string;
    namespace: string;
  };
};

type ProjectCommandProps = {
  mode?: "project";
  organizationSlug: string;
  initialPanel?: InitialPanel;
  onCreated?: (result: ProjectCreatedResult) => void | Promise<void>;
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
  onCreated?: (
    result: Awaited<ReturnType<typeof createServiceServerFn>>["data"],
  ) => void | Promise<void>;
  onCreateVariableGroup?: () => void;
  onCreateVolume?: () => void;
};

type ServiceCreateCommandProps = ProjectCommandProps | ServiceCommandProps;

type RootPanelProps = {
  mode: CreateMode;
  onSelectItem: (itemId: CreateMenuItemId) => void;
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
  const items = getCreateMenuItems({ includeEmptyProject: mode === "project" }).filter(({ id }) =>
    mode === "service" || id === "git-repository" || id === "container-image" || id === "empty-project",
  );
  return (
    <CommandGroup>
      {items.map(
        ({ id, icon: Icon, label }) => (
          <CommandItem
            key={id}
            value={label}
            keywords={["create", mode]}
            disabled={isPending}
            onSelect={() => onSelectItem(id)}
          >
            <Icon />
            <span>{label}</span>
            {id === "git-repository" || id === "container-image" ? (
              <CommandShortcut>
                <ChevronRightIcon />
              </CommandShortcut>
            ) : isPending ? (
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
  const createEmptyProject = useServerFn(createEmptyProjectServerFn);
  const createService = useServerFn(createServiceServerFn);
  const createVariableGroupResource = useServerFn(
    createVariableGroupResourceServerFn,
  );
  const createVolumeResource = useServerFn(createVolumeResourceServerFn);

  const createEmptyProjectMutation = useMutation({
    mutationFn: () =>
      createEmptyProject({
        data: {
          organizationSlug: props.organizationSlug,
        },
      }),
    onSuccess: async (receipt) => {
      await Promise.all([
        getProjectsCollection(props.organizationSlug, collectionScope).writeCommitted(receipt.data.project),
        getEnvironmentsCollection(props.organizationSlug, collectionScope).writeCommitted(receipt.data.environment),
        getEnvironmentSummariesCollection(props.organizationSlug, collectionScope).writeCommitted(environmentSummary(receipt.data.environment)),
      ]);
      if (props.mode !== "service") {
        await props.onCreated?.(receipt.data);
      }
    },
  });

  const createServiceMutation = useMutation({
    mutationFn: async (input: {
      environmentId: string;
      source: ServiceSource;
      canvasPosition: { x: number; y: number };
    }) => {
      return createService({
        data: {
          organizationSlug: props.organizationSlug,
          environmentId: input.environmentId,
          x: input.canvasPosition.x,
          y: input.canvasPosition.y,
          source: input.source,
        },
      });
    },
    onSuccess: async (result) => {
      await applyCreatedService(props.organizationSlug, collectionScope, result.data);
      if (props.mode === "service") {
        await props.onCreated?.(result.data);
      }
    },
  });
  const createVariableGroupMutation = useMutation({
    mutationFn: (input: { environmentId: string }) =>
      createVariableGroupResource({
        data: {
          organizationSlug: props.organizationSlug,
          environmentId: input.environmentId,
          name: "Database",
          x: 0,
          y: 0,
        },
      }),
    onSuccess: async (result) => {
      await applyCreatedResource(props.organizationSlug, collectionScope, result);
    },
  });
  const createVolumeMutation = useMutation({
    mutationFn: (input: { environmentId: string }) =>
      createVolumeResource({
        data: {
          organizationSlug: props.organizationSlug,
          environmentId: input.environmentId,
          name: "data",
          x: 0,
          y: 0,
        },
      }),
    onSuccess: async (result) => {
      await applyCreatedResource(props.organizationSlug, collectionScope, result);
    },
  });

  function resetPanelState() {
    setQuery("");
    createEmptyProjectMutation.reset();
    createServiceMutation.reset();
    createVariableGroupMutation.reset();
    createVolumeMutation.reset();
  }

  function setActivePanel(nextPanel: InitialPanel) {
    resetPanelState();
    setPanel({ kind: nextPanel });
  }

  const isPending =
    createEmptyProjectMutation.isPending ||
    createServiceMutation.isPending ||
    createVariableGroupMutation.isPending ||
    createVolumeMutation.isPending;
  const error =
    createEmptyProjectMutation.error ??
    createServiceMutation.error ??
    createVariableGroupMutation.error ??
    createVolumeMutation.error;

  async function getServiceModeEnvironment() {
    if (props.mode !== "service") {
      throw new Error("Service mode environment is unavailable in project mode");
    }

    const environment = await loadWorkspaceEnvironment(props, collectionScope);
    if (!environment) {
      throw new Error("Environment not found");
    }

    return {
      projectSlug: props.projectSlug,
      environmentSlug: props.environmentSlug,
      environmentId: environment.id,
      canvasPosition: props.canvasPosition,
    };
  }

  async function createProjectTarget() {
    const receipt = await createEmptyProjectMutation.mutateAsync();
    return {
      projectSlug: receipt.data.project.slug,
      environmentSlug: receipt.data.environment.namespace,
      environmentId: receipt.data.environment.id,
      canvasPosition: { x: 0, y: 0 },
    };
  }

  async function getCreationTarget() {
    return props.mode === "service"
      ? getServiceModeEnvironment()
      : createProjectTarget();
  }

  async function navigateToEnvironment(target: {
    projectSlug: string;
    environmentSlug: string;
  }) {
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

  async function createServiceFromSource(source: ServiceSource) {
    const target = await getCreationTarget();
    const result = await createServiceMutation.mutateAsync({
      environmentId: target.environmentId,
      canvasPosition: target.canvasPosition,
      source,
    });

    if (props.mode !== "service") {
      await navigate({
        to: ENVIRONMENT_SERVICE_ROUTE_TO,
        params: {
          organizationSlug: props.organizationSlug,
          projectSlug: target.projectSlug,
          environmentSlug: target.environmentSlug,
          serviceId: result.data.service.id,
        },
        search: (prev) => prev,
      });
    }
  }

  async function createVariableGroup() {
    if (props.mode === "service" && props.onCreateVariableGroup) {
      props.onCreateVariableGroup();
      return;
    }

    const target = await getCreationTarget();
    const result = await createVariableGroupMutation.mutateAsync({
      environmentId: target.environmentId,
    });

    await navigate({
      to: ENVIRONMENT_RESOURCE_ROUTE_TO,
      params: {
        organizationSlug: props.organizationSlug,
        projectSlug: target.projectSlug,
        environmentSlug: target.environmentSlug,
          resourceId: result.data.resource.id,
      },
      search: (prev) => prev,
    });
  }

  async function createVolume() {
    if (props.mode === "service" && props.onCreateVolume) {
      props.onCreateVolume();
      return;
    }

    const target = await getCreationTarget();
    const result = await createVolumeMutation.mutateAsync({
      environmentId: target.environmentId,
    });

    await navigate({
      to: ENVIRONMENT_RESOURCE_ROUTE_TO,
      params: {
        organizationSlug: props.organizationSlug,
        projectSlug: target.projectSlug,
        environmentSlug: target.environmentSlug,
          resourceId: result.data.resource.id,
      },
      search: (prev) => prev,
    });
  }

  function selectCreateItem(itemId: CreateMenuItemId) {
    if (itemId === "git-repository") {
      setActivePanel("git");
      return;
    }
    if (itemId === "container-image") {
      setActivePanel("image");
      return;
    }
    if (itemId === "empty-service") {
      void createServiceFromSource(createEmptyServiceSource());
      return;
    }
    if (itemId === "variable-group") {
      void createVariableGroup();
      return;
    }
    if (itemId === "volume") {
      void createVolume();
      return;
    }

    void createProjectTarget().then(navigateToEnvironment);
  }

  return {
    error,
    isPending,
    selectCreateItem,
    setActivePanel,
    createServiceFromSource,
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
            void createServiceFromSource(createImageServiceSource({ image }));
          }}
        />
      ) : (
      <SourcePickerLayout title={presentation.title}>
      <Command key={panel.kind} shouldFilter={panel.kind === "root"}>
        <SourcePickerInput
            onBack={
              panel.kind === "git"
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
              onSelectRepo={({
                fullName,
                repositoryId,
                access,
                defaultBranch,
              }) => {
                void createServiceFromSource(
                  createGitServiceSource({
                    repository: fullName,
                    repositoryId,
                    access,
                    branch: { type: "connected", name: defaultBranch },
                  }),
                );
              }}
            />
          ) : null}
        </CommandList>
      </Command>
      </SourcePickerLayout>
      )}
    </div>
  );
}
