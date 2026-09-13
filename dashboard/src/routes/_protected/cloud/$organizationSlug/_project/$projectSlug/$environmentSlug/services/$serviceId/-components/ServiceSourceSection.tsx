import { useState } from "react";
import {
  GitRepoSelectorDialog,
  ImageSelectorDialog,
} from "#/components/service-source-selector";
import { Button } from "#/components/ui/button";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import {
  Item,
  ItemActions,
  ItemContent,
  ItemMedia,
  ItemTitle,
} from "#/components/ui/item";
import { ArrowUpRightIcon, PackageIcon, PencilIcon } from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import {
  createEmptyServiceSource,
  createGitServiceSource,
  createImageServiceSource,
  serviceRootDirSchema,
} from "#/modules/environment-design/services";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import { SchemaFieldInput } from "#/components/stageable/schema-field-input";
import { GitBranchSettings } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/GitBranchSettings";
import { ServiceRegistryCredentialsSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialsSection";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceSourceSection({ state }: { state: ServiceDrawerState }) {
  const { service } = state;

  if (service.source.type === "git") {
    return <GitServiceSourceSection state={state} source={service.source} />;
  }

  if (service.source.type === "empty") {
    return <EmptyServiceSourceSection state={state} />;
  }

  if (service.source.type === "image") {
    return <ImageServiceSourceSection state={state} />;
  }

  return null;
}

function GitServiceSourceSection({
  state,
  source,
}: {
  state: ServiceDrawerState;
  source: Extract<ServiceDrawerState["service"]["source"], { type: "git" }>;
}) {
  const { service, diff, collection } = state;
  const [isGitRepoSelectorOpen, setIsGitRepoSelectorOpen] = useState(false);
  const [isGitRootDirVisible, setIsGitRootDirVisible] = useState(
    source.rootDir !== "/" ||
      diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.sourceRootDir).baselineValue !==
        "/"
  );

  const repositoryUrl = `https://github.com/${source.repository}`;
  const repositoryDiff = diff.field(
    SERVICE_DEPLOYMENT_DIFF_PATHS.sourceRepository
  );
  const rootDirDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.sourceRootDir);

  return (
    <FieldGroup>
      <Field>
        <FieldLabel>Repository</FieldLabel>
        <Item
          variant="muted"
          data-changed={repositoryDiff.changed || undefined}
          title={
            repositoryDiff.changed
              ? `${repositoryDiff.baselineLabel}: ${repositoryDiff.baselineValue ?? ""}`
              : undefined
          }
        >
          <ItemMedia variant="icon">
            <GitHubMarkIcon />
          </ItemMedia>
          <ItemContent>
            <ItemTitle>
              <a href={repositoryUrl} target="_blank" rel="noreferrer">
                {source.repository}
              </a>
            </ItemTitle>
          </ItemContent>
          <ItemActions>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              onClick={() => setIsGitRepoSelectorOpen(true)}
            >
              <PencilIcon />
              <span className="sr-only">Edit repository</span>
            </Button>
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                const transaction = collection.update(service.id, (draft) => {
                  if (draft.source.type !== "git") {
                    return;
                  }

                  draft.source = createEmptyServiceSource();
                });

                void transaction.isPersisted.promise;
              }}
            >
              Disconnect
            </Button>
          </ItemActions>
        </Item>
      </Field>

      <Field>
        {!isGitRootDirVisible ? (
          <div className="flex flex-wrap items-center gap-1 text-sm text-muted-foreground">
            <Button
              type="button"
              variant="link"
              size="sm"
              onClick={() => setIsGitRootDirVisible(true)}
            >
              Add root directory
            </Button>
            <span>(used for build and deploy steps.)</span>
          </div>
        ) : (
          <div className="flex flex-col gap-1">
            <FieldLabel htmlFor="service-git-root-dir">
              Root directory
            </FieldLabel>
            <FieldDescription>
              Choose where Ployz should look for your code.{" "}
              <Button type="button" variant="link" size="sm" disabled>
                Docs
                <ArrowUpRightIcon data-icon="inline-end" />
              </Button>
            </FieldDescription>
            <SchemaFieldInput
              schema={serviceRootDirSchema}
              value={source.rootDir}
              baselineLabel={rootDirDiff.baselineLabel}
              baselineValue={rootDirDiff.baselineValue}
              isChanged={rootDirDiff.changed}
              label="Root directory"
              onCommit={(rootDir) =>
                collection.update(service.id, (draft) => {
                  if (draft.source.type !== "git") {
                    return;
                  }

                  draft.source.rootDir = rootDir;
                })
              }
            />
          </div>
        )}
      </Field>

      <GitBranchSettings state={state} source={source} />

      <GitRepoSelectorDialog
        open={isGitRepoSelectorOpen}
        onOpenChange={setIsGitRepoSelectorOpen}
        onSelectRepo={async ({
          fullName,
          repositoryId,
          installationId,
          defaultBranch,
        }) => {
          const transaction = collection.update(service.id, (draft) => {
            if (draft.source.type !== "git") {
              return;
            }

            draft.source = createGitServiceSource({
              repository: fullName,
              repositoryId,
              installationId,
              rootDir: draft.source.rootDir,
              branch: {
                type: "connected",
                name: defaultBranch,
              },
              autoDeploy: draft.source.autoDeploy,
              waitForCi: draft.source.waitForCi,
            });
          });

          await transaction.isPersisted.promise;
        }}
      />
    </FieldGroup>
  );
}

function EmptyServiceSourceSection({ state }: { state: ServiceDrawerState }) {
  const { service, diff, collection } = state;
  const source = service.source.type === "empty" ? service.source : null;
  const [isGitRepoSelectorOpen, setIsGitRepoSelectorOpen] = useState(false);
  const [isImageSelectorOpen, setIsImageSelectorOpen] = useState(false);
  const rootDirDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.sourceRootDir);

  if (!source) {
    return null;
  }

  return (
    <FieldGroup>
      <div className="flex flex-col gap-5">
        <div className="flex flex-col gap-2">
          <div>
            <h3 className="text-base font-semibold">Add a source</h3>
            <p className="text-sm text-muted-foreground">
              Choose a GitHub repository or container image.
            </p>
          </div>
          <div className="flex flex-wrap gap-3">
            <Button
              type="button"
              variant="outline"
              onClick={() => setIsGitRepoSelectorOpen(true)}
            >
              <GitHubMarkIcon data-icon="inline-start" />
              Git repository
            </Button>
            <Button
              type="button"
              variant="outline"
              onClick={() => setIsImageSelectorOpen(true)}
            >
              <PackageIcon data-icon="inline-start" />
              Container image
            </Button>
          </div>
        </div>

        <Field>
          <div className="flex flex-col gap-1">
            <FieldLabel htmlFor="service-root-dir">Root directory</FieldLabel>
            <FieldDescription>
              Choose the directory to deploy when you use the CLI.
            </FieldDescription>
            <div>
              <Button
                type="button"
                variant="link"
                size="sm"
                disabled
                className="h-auto px-0 text-muted-foreground"
              >
                Docs
                <ArrowUpRightIcon data-icon="inline-end" />
              </Button>
            </div>
          </div>
          <SchemaFieldInput
            schema={serviceRootDirSchema}
            value={source.rootDir}
            baselineLabel={rootDirDiff.baselineLabel}
            baselineValue={rootDirDiff.baselineValue}
            isChanged={rootDirDiff.changed}
            label="Root directory"
            onCommit={(rootDir) =>
              collection.update(service.id, (draft) => {
                if (draft.source.type !== "empty") {
                  return;
                }

                draft.source.rootDir = rootDir;
              })
            }
          />
        </Field>
      </div>

      <GitRepoSelectorDialog
        open={isGitRepoSelectorOpen}
        onOpenChange={setIsGitRepoSelectorOpen}
        onSelectRepo={async ({
          fullName,
          repositoryId,
          installationId,
          defaultBranch,
        }) => {
          const transaction = collection.update(service.id, (draft) => {
            if (draft.source.type !== "empty") {
              return;
            }

            draft.source = createGitServiceSource({
              repository: fullName,
              repositoryId,
              installationId,
              rootDir: draft.source.rootDir,
              branch: {
                type: "connected",
                name: defaultBranch,
              },
            });
          });

          await transaction.isPersisted.promise;
        }}
      />
      <ImageSelectorDialog
        open={isImageSelectorOpen}
        onOpenChange={setIsImageSelectorOpen}
        onSelectImage={async (image) => {
          const transaction = collection.update(service.id, (draft) => {
            if (draft.source.type !== "empty") {
              return;
            }

            draft.source = createImageServiceSource({
              image,
            });
          });

          await transaction.isPersisted.promise;
        }}
      />
    </FieldGroup>
  );
}

function ImageServiceSourceSection({ state }: { state: ServiceDrawerState }) {
  const { service, diff, collection } = state;
  const source = service.source.type === "image" ? service.source : null;
  const [isImageSelectorOpen, setIsImageSelectorOpen] = useState(false);

  if (!source) {
    return null;
  }

  const imageDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.sourceImage);

  return (
    <FieldGroup>
      <Field>
        <FieldLabel>Container image</FieldLabel>
        <Item
          variant="muted"
          data-changed={imageDiff.changed || undefined}
          title={
            imageDiff.changed
              ? `${imageDiff.baselineLabel}: ${imageDiff.baselineValue ?? ""}`
              : undefined
          }
        >
          <ItemMedia variant="icon">
            <PackageIcon />
          </ItemMedia>
          <ItemContent>
            <ItemTitle>{source.image}</ItemTitle>
          </ItemContent>
          <ItemActions>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              onClick={() => setIsImageSelectorOpen(true)}
            >
              <PencilIcon />
              <span className="sr-only">Edit image</span>
            </Button>
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                const transaction = collection.update(service.id, (draft) => {
                  if (draft.source.type !== "image") {
                    return;
                  }

                  draft.source = createEmptyServiceSource();
                });

                void transaction.isPersisted.promise;
              }}
            >
              Disconnect
            </Button>
          </ItemActions>
        </Item>
      </Field>

      <ServiceRegistryCredentialsSection state={state} />

      <ImageSelectorDialog
        open={isImageSelectorOpen}
        onOpenChange={setIsImageSelectorOpen}
        onSelectImage={async (image) => {
          const transaction = collection.update(service.id, (draft) => {
            if (draft.source.type !== "image") {
              return;
            }

            draft.source = createImageServiceSource({
              image,
              autoUpdate: draft.source.autoUpdate,
              credentials: draft.source.credentials,
            });
          });

          await transaction.isPersisted.promise;
        }}
      />
    </FieldGroup>
  );
}
