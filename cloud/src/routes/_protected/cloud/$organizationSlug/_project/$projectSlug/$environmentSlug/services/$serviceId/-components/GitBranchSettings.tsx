import { useState } from "react";
import {
  ChevronDownIcon,
  GitBranchIcon,
  ZapIcon,
  ZapOffIcon,
} from "lucide-react";
import { GitBranchSelectorDialog } from "#/components/service-source-selector";
import { Button } from "#/components/ui/button";
import {
  Field,
  FieldDescription,
  FieldLabel,
} from "#/components/ui/field";
import {
  Item,
  ItemActions,
  ItemContent,
  ItemMedia,
  ItemTitle,
} from "#/components/ui/item";
import { Separator } from "#/components/ui/separator";
import { Switch } from "#/components/ui/switch";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import {
  createGitServiceSource,
  type ServiceSource,
} from "#/modules/environment-design/services";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

type GitServiceSource = Extract<ServiceSource, { type: "git" }>;

export function GitBranchSettings({
  state,
  source,
}: {
  state: ServiceDrawerState;
  source: GitServiceSource;
}) {
  const { service, diff, collection } = state;
  const [isGitBranchSelectorOpen, setIsGitBranchSelectorOpen] = useState(false);
  const branch = source.branch;
  const isBranchConnected = branch.type === "connected";
  const branchLabel = isBranchConnected
    ? branch.name
    : (branch.previousName ?? "Disconnected");
  const branchDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.sourceBranch);
  const waitForCiDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.sourceWaitForCi);
  const autoDeployEnabled = source.autoDeploy;

  return (
    <>
      <Field>
        <div className="flex flex-col gap-2">
          <FieldLabel>Branch</FieldLabel>
          <FieldDescription>
            Deploy changes from this branch to this environment.
          </FieldDescription>
        </div>
        {isBranchConnected ? (
          <div className="overflow-hidden rounded-lg border bg-card">
            <Item
              data-changed={branchDiff.changed || undefined}
              title={
                branchDiff.changed
                  ? `${branchDiff.baselineLabel}: ${branchDiff.baselineValue ?? "Disconnected"}`
                  : undefined
              }
            >
              <ItemMedia variant="icon">
                <GitBranchIcon />
              </ItemMedia>
              <ItemContent>
                <Button
                  type="button"
                  variant="ghost"
                  className="w-full justify-between"
                  onClick={() => setIsGitBranchSelectorOpen(true)}
                >
                  {branchLabel}
                  <ChevronDownIcon data-icon="inline-end" />
                </Button>
              </ItemContent>
              <ItemActions>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => {
                    const transaction = collection.update(service.id, (draft) => {
                      if (draft.source.type !== "git") {
                        return;
                      }

                      draft.source = createGitServiceSource({
                        repository: draft.source.repository,
                        repositoryId: draft.source.repositoryId,
                        installationId: draft.source.installationId,
                        rootDir: draft.source.rootDir,
                        branch: {
                          type: "disconnected",
                          previousName:
                            draft.source.branch.type === "connected"
                              ? draft.source.branch.name
                              : draft.source.branch.previousName,
                        },
                        autoDeploy: draft.source.autoDeploy,
                        waitForCi: draft.source.waitForCi,
                      });
                    });

                    void transaction.isPersisted.promise;
                  }}
                >
                  Disconnect
                </Button>
              </ItemActions>
            </Item>
            <Separator />
            <Item variant={autoDeployEnabled ? "default" : "muted"}>
              <ItemMedia variant="icon">
                {autoDeployEnabled ? <ZapIcon /> : <ZapOffIcon />}
              </ItemMedia>
              <ItemContent>
                <ItemTitle>
                  {autoDeployEnabled
                    ? "Auto deploys when pushed to GitHub"
                    : "Auto deploy is disabled"}
                </ItemTitle>
              </ItemContent>
              <ItemActions>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => {
                    const transaction = collection.update(service.id, (draft) => {
                      if (draft.source.type !== "git") {
                        return;
                      }

                      draft.source.autoDeploy = !draft.source.autoDeploy;
                    });

                    void transaction.isPersisted.promise;
                  }}
                >
                  {autoDeployEnabled ? "Disable" : "Enable"}
                </Button>
              </ItemActions>
            </Item>
          </div>
        ) : (
          <Button
            type="button"
            variant="outline"
            data-changed={branchDiff.changed || undefined}
            onClick={() => setIsGitBranchSelectorOpen(true)}
          >
            Connect branch
          </Button>
        )}
      </Field>

      {isBranchConnected ? (
        <Field>
          <div className="flex flex-col gap-2">
            <FieldLabel>Deploy after CI passes</FieldLabel>
            <FieldDescription>
              Wait for GitHub Actions to pass before deploying.
            </FieldDescription>
          </div>
          <Item variant="muted" data-changed={waitForCiDiff.changed || undefined}>
            <ItemActions>
              <Switch
                checked={source.waitForCi}
                onCheckedChange={(nextChecked) => {
                  const transaction = collection.update(service.id, (draft) => {
                    if (draft.source.type !== "git") {
                      return;
                    }

                    draft.source.waitForCi = nextChecked;
                  });

                  void transaction.isPersisted.promise;
                }}
              />
            </ItemActions>
            <ItemContent>
              <ItemTitle>Deploy after CI passes</ItemTitle>
            </ItemContent>
          </Item>
        </Field>
      ) : null}

      <GitBranchSelectorDialog
        open={isGitBranchSelectorOpen}
        onOpenChange={setIsGitBranchSelectorOpen}
        repositoryFullName={source.repository}
        repositoryId={source.repositoryId}
        installationId={source.installationId}
        onSelectBranch={async (branchName) => {
          const transaction = collection.update(service.id, (draft) => {
            if (draft.source.type !== "git") {
              return;
            }

            draft.source = createGitServiceSource({
              repository: draft.source.repository,
              repositoryId: draft.source.repositoryId,
              installationId: draft.source.installationId,
              rootDir: draft.source.rootDir,
              branch: {
                type: "connected",
                name: branchName,
              },
              autoDeploy: draft.source.autoDeploy,
              waitForCi: draft.source.waitForCi,
            });
          });

          await transaction.isPersisted.promise;
        }}
      />
    </>
  );
}
