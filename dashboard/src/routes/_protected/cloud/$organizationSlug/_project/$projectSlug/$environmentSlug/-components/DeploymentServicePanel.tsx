import { Navigate, useNavigate, useParams, useSearch } from "@tanstack/react-router";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { Schema } from "effect";
import type { ServiceConfig } from "@ployz/sdk/config";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getRawServicesCollection } from "#/collections/collections";
import { ServiceBuildLogs, ServiceDeployLogs } from "#/components/deployment-logs";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import type { DeploymentAttempt } from "#/modules/deployments/deployment.collection";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import { outcomeBadges } from "#/components/deployment-outcome-badges";
import { builtOnLine, nodeOutcomeLabels, shortDeploymentId, type DeploymentNodeView } from "#/modules/deployments/deployment-view";
import {
  DEPLOYMENT_SERVICE_PAGES, deploymentServicePageSchema, type DeploymentServicePage,
} from "../services/$serviceId/-components/service-pages";
import { CanvasInspectorHeader } from "./CanvasInspectorHeader";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";

/** The tab that matters for the node: the build or rollout that failed or is running, otherwise Details. */
export function defaultDeploymentTab(view: DeploymentNodeView): DeploymentServicePage {
  if (view.build.state === "failed" || view.build.state === "running") return "build-logs";
  if (view.deploy.state === "failed" || view.deploy.state === "running") return "deploy-logs";
  return "details";
}

/** The service panel in Deployment Mode: read-only, as the attempt deployed the service. */
export function DeploymentServicePanel({ attempt, serviceId }: { attempt: DeploymentAttempt; serviceId: string }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { tab } = useSearch({ strict: false });
  const navigate = useNavigate();
  const services = getRawServicesCollection(params.organizationSlug, useCollectionScope());
  const { data: named } = useLiveSuspenseQuery({
    queryKey: ["deployment-panel-service", services.id, serviceId],
    query: (q) => q.from({ service: services }).where(({ service }) => eq(service.id, serviceId)).select(({ service }) => ({ name: service.name })),
  });
  const node = attempt.nodes.find((candidate) => candidate.nodeId === serviceId);
  const view = attempt.view.nodes.find((candidate) => candidate.nodeId === serviceId);
  if (node?.nodeType !== "service" || !view) return null;
  const { config } = node;
  const built = view.build.state !== "none";
  const requested = Schema.is(deploymentServicePageSchema)(tab) && (built || tab !== "build-logs") ? tab : null;
  const current = requested ?? defaultDeploymentTab(view);
  const destination = { to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId } } as const;
  const { deployment } = attempt;

  return (
    <div className="flex h-full min-h-0 flex-col">
      {requested ? null : <Navigate {...destination} search={(previous) => ({ ...previous, tab: current })} replace />}
      <CanvasInspectorHeader params={params}>
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate font-medium">{named[0]?.name ?? config.privateDns}</span>
          {/* The bar already names the deployment; a phone keeps the width for the service name. */}
          <span className="whitespace-nowrap text-muted-foreground max-[860px]:hidden">
            <span aria-hidden>/ </span><span className="font-mono">{shortDeploymentId(deployment.id)}</span>
          </span>
          <Badge variant={outcomeBadges[view.outcome]}>{nodeOutcomeLabels[view.outcome]}</Badge>
        </div>
      </CanvasInspectorHeader>
      <div className="flex min-h-0 flex-1 flex-col px-4 pb-4">
        <Tabs
          value={current}
          onValueChange={(value) => {
            if (Schema.is(deploymentServicePageSchema)(value)) {
              void navigate({ ...destination, search: (previous) => ({ ...previous, tab: value }), replace: true });
            }
          }}
          className="flex min-h-0 flex-1 flex-col overflow-hidden"
        >
          <TabsList variant="line" className="max-w-full shrink-0 overflow-x-auto max-[860px]:hidden">
            {DEPLOYMENT_SERVICE_PAGES.map((page) => {
              const disabled = page.id === "build-logs" && !built;
              return <TabsTrigger key={page.id} value={page.id} disabled={disabled} title={disabled ? "Prebuilt image, nothing built" : undefined}>{page.label}</TabsTrigger>;
            })}
          </TabsList>
          <TabsContent value="details" className="mt-4 overflow-y-auto">
            <DeploymentServiceDetails view={view} config={config} commitSha={deployment.sourcePins[serviceId]?.commitSha ?? null} />
          </TabsContent>
          <TabsContent value="build-logs" className="mt-4 overflow-y-auto">
            <ServiceBuildLogs organizationSlug={params.organizationSlug} deploymentId={deployment.id} image={config.privateDns} />
          </TabsContent>
          <TabsContent value="deploy-logs" className="mt-4 flex min-h-0 flex-1 flex-col">
            {view.outcome === "not_attempted" || view.outcome === "unchanged" ? <p className="mb-3 text-muted-foreground">{outcomeSentences[view.outcome]}</p> : null}
            <ServiceDeployLogs organizationSlug={params.organizationSlug} deploymentId={deployment.id} serviceId={serviceId} finished={!isActiveDeployment(deployment.status)} />
          </TabsContent>
        </Tabs>
      </div>
    </div>
  );
}

const outcomeSentences = {
  deployed: "Deployed · the new container replaced the previous one",
  removed: "Removed by this deployment",
  not_attempted: "Not attempted · an earlier failure stopped the rollout; the previous version kept running",
  unchanged: "Unchanged in this deployment · the previous version kept running",
  queued: "Queued in the rollout",
  building: "Building",
  deploying: "Deploying",
} satisfies Record<Exclude<DeploymentNodeView["outcome"], "failed">, string>;

const restartPolicies = { "unless-stopped": "Unless stopped", always: "Always", "on-failure": "On failure", no: "Never" } satisfies Record<ServiceConfig["restartPolicy"], string>;

function Fields({ title, fields }: { title: string; fields: [label: string, value: string | null][] }) {
  return (
    <section className="flex flex-col gap-2">
      <h3 className="font-medium">{title}</h3>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
        {fields.flatMap(([label, value]) => value === null ? [] : [
          <dt key={`${label}:dt`} className="text-muted-foreground">{label}</dt>,
          <dd key={`${label}:dd`} className="min-w-0 break-all font-mono">{value}</dd>,
        ])}
      </dl>
    </section>
  );
}

function DeploymentServiceDetails({ view, config, commitSha }: { view: DeploymentNodeView; config: ServiceConfig; commitSha: string | null }) {
  const { source, build, healthcheck } = config;
  const variables = Object.entries(config.env).sort(([a], [b]) => a.localeCompare(b));
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-6">
      {view.failure ? (
        <Alert variant="destructive">
          <AlertTitle>Failed</AlertTitle>
          <AlertDescription>
            <pre className="whitespace-pre-wrap break-words font-mono">{view.failure.message}</pre>
            {view.failure.containerId ? <p>Container <span className="font-mono">{view.failure.containerId}</span></p> : null}
          </AlertDescription>
        </Alert>
      ) : (
        <p>{view.outcome === "failed" ? "Failed" : outcomeSentences[view.outcome]}</p>
      )}
      <details>
        <summary className="cursor-pointer font-medium">{variables.length} {variables.length === 1 ? "variable" : "variables"} (as deployed)</summary>
        <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 font-mono text-xs">
          {variables.flatMap(([key, value]) => [
            <dt key={`${key}:dt`}>{key}</dt>,
            <dd key={`${key}:dd`} className="min-w-0 break-all text-muted-foreground">{value.kind === "secret" ? "••••••••" : value.value}</dd>,
          ])}
        </dl>
      </details>
      <Fields title="Source" fields={source.type === "image" ? [["Image", source.image]]
        : source.type === "git" ? [
          ["Repository", source.repository],
          ["Branch", source.branch.type === "connected" ? source.branch.name : source.branch.previousName],
          ["Commit", commitSha],
          ["Root directory", source.rootDir || null],
        ]
        : [["Source", "None"]]} />
      {source.type === "git" ? <Fields title="Build" fields={[
        ["Built on", view.builtOn ? builtOnLine(view.builtOn) : null],
        ["Build method", build.buildMethod === "dockerfile" ? "Dockerfile" : "Railpack"],
        ["Dockerfile", build.dockerfilePath],
      ]} /> : null}
      <Fields title="Deploy" fields={[
        ["Replicas", String(config.replicas)],
        ["Start command", config.startCommand],
        ["Pre-deploy", config.preDeployCommand],
        ["Health check", healthcheck.type === "http" ? `HTTP ${healthcheck.path} · ${healthcheck.timeoutSeconds}s` : "None"],
        ["Restart", config.restartPolicy === "on-failure" ? `${restartPolicies[config.restartPolicy]} · ${config.maxRetries} retries` : restartPolicies[config.restartPolicy]],
        ["CPU limit", config.cpuLimit === null ? null : String(config.cpuLimit)],
        ["Memory limit", config.memLimit === null ? null : String(config.memLimit)],
      ]} />
    </div>
  );
}
