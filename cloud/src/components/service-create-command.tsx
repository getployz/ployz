import { applyCreatedService, applyCreatedResource } from "#/modules/environment-design/apply-created-node";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useState } from "react";
import { ArrowLeftIcon, ChevronRightIcon } from "lucide-react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import {
  Command,
  CommandGroup,
  CommandInput,
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
  environmentBySlugQueryOptions,
} from "#/modules/environment-design/workspace-queries";
import { createEmptyProjectServerFn } from "#/modules/environment-design/workspace-functions";
import {
  createEmptyServiceSource,
  createGitServiceSource,
  createImageServiceSource,
  type ServiceSource,
} from "#/modules/environment-design/services";
import {
  getEnvironmentsCollection,
  getProjectsCollection,
} from "#/collections/collections";
import { createServiceServerFn } from "#/modules/environment-design/service-functions";
import {
  ENVIRONMENT_INDEX_ROUTE_TO,
  ENVIRONMENT_RESOURCE_ROUTE_TO,
  ENVIRONMENT_SERVICE_ROUTE_TO,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";

type Panel = "root" | "git" | "image";
type CreateMode = "project" | "service";

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
  initialPanel?: Panel;
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
  initialPanel?: Panel;
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
    installationId: number;
    defaultBranch: string;
  }) => void;
};

type GitPanelProps = GitPanelReposProps;

type ImagePanelProps = {
  disabled: boolean;
  onCreateImage: (image: string) => void;
};

function RootPanel({ mode, onSelectItem, isPending }: RootPanelProps) {
  return (
    <CommandGroup>
      {getCreateMenuItems({ includeEmptyProject: mode === "project" }).map(
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

function ImagePanel({ disabled, onCreateImage }: ImagePanelProps) {
  return <ImageSelector disabled={disabled} onSelectImage={onCreateImage} />;
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
  const queryClient = useQueryClient();
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

  function setActivePanel(nextPanel: Panel) {
    resetPanelState();
    setPanel(nextPanel);
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

    const environment = await queryClient.ensureQueryData(
      environmentBySlugQueryOptions(
        props.organizationSlug,
        props.projectSlug,
        props.environmentSlug,
      ),
    );
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
  const [panel, setPanel] = useState<Panel>(props.initialPanel ?? "root");
  const [query, setQuery] = useState("");
  const showHeader = panel !== "root";
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
        if (panel === "root" || event.key !== "Escape") {
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

      {showHeader ? (
        <div>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setActivePanel("root");
            }}
          >
            <ArrowLeftIcon data-icon="inline-start" />
            Back
          </Button>
        </div>
      ) : null}

      <Command shouldFilter={panel !== "git"}>
        <CommandInput
          aria-label={
            panel === "git"
              ? "Search GitHub repositories"
              : panel === "image"
                ? "Search container images"
                : mode === "service"
                  ? "Choose a service type"
                  : "Choose what to add"
          }
          disabled={isPending}
          value={query}
          onValueChange={setQuery}
          placeholder={
            panel === "git"
              ? "Search GitHub repositories…"
              : panel === "image"
                ? "Search container images…"
                : mode === "service"
                  ? "Choose a service type…"
                  : "Choose what to add…"
          }
        />
        <CommandList>
          {panel === "root" ? (
            <RootPanel
              mode={mode}
              onSelectItem={selectCreateItem}
              isPending={isPending}
            />
          ) : null}
          {panel === "git" ? (
            <GitPanel
              mode={mode}
              query={query}
              disabled={isPending}
              onSelectRepo={({
                fullName,
                repositoryId,
                installationId,
                defaultBranch,
              }) => {
                void createServiceFromSource(
                  createGitServiceSource({
                    repository: fullName,
                    repositoryId,
                    installationId,
                    branch: {
                      type: "connected",
                      name: defaultBranch,
                    },
                  }),
                );
              }}
            />
          ) : null}
          {panel === "image" ? (
            <ImagePanel
              disabled={isPending}
              onCreateImage={(image) => {
                void createServiceFromSource(
                  createImageServiceSource({ image }),
                );
              }}
            />
          ) : null}
        </CommandList>
      </Command>
    </div>
  );
}
