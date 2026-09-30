import type { QueryClient } from "@tanstack/react-query";
import type {
  BranchView, ConfigCommand, ConfigQuery, DeploymentView, DiffView, DomainsView, EnvironmentRef, EnvironmentsView, EnvironmentView,
  NodeChange, ServiceListing, ServicesView, VolumeListing, VolumesView,
} from "@ployz/sdk";
import type { StoreResult } from "./store.contract";
import { environmentKey, queryOf, storeViewPrefix } from "./store-view.queries";

/**
 * Shows a Store command in the cached views at once, as the Store will answer once it commits: what the user sees while
 * it saves. The writer's refetch after the commit (or its refusal, which is the rollback) replaces the guess. It guesses
 * only what the command says outright; anything the Store derives (a rename's diff rows, a Move's changes) waits.
 */
// ponytail: Discard shows the deployed values (`before`), which is what it restores unless changes were published first.
export function applyOptimistic(queryClient: QueryClient, organizationSlug: string, command: ConfigCommand) {
  const cached = (kind: ConfigQuery["query"], environment: EnvironmentRef | null) =>
    queryClient.getQueryCache().findAll({ queryKey: storeViewPrefix(organizationSlug) }).filter((query) => {
      const config = queryOf(query);
      return config?.query === kind
        && (!environment || ("environment" in config && environmentKey(config.environment) === environmentKey(environment)));
    });
  const views = <V,>(kind: ConfigQuery["query"], environment: EnvironmentRef | null, update: (view: V) => V) => {
    for (const query of cached(kind, environment)) {
      queryClient.setQueryData<StoreResult<V>>(query.queryKey, (old) => old?.ok ? { ok: true, value: update(old.value) } : old);
    }
  };
  // A node gone from the review is back to how it is deployed: a new one goes, a removal stays.
  const unstage = <L extends { name: string; change: ServiceListing["change"] }>(listed: L[], names: ReadonlySet<string>) =>
    listed.flatMap((node) => !names.has(node.name) ? [node] : node.change === "create" ? [] : [{ ...node, change: null }]);

  switch (command.command) {
    case "batch":
      for (const inner of command.commands) applyOptimistic(queryClient, organizationSlug, inner);
      return;
    case "create_service":
    case "create_git_service": {
      const service: ServiceListing = {
        id: command.id, name: command.name, private_dns: command.name, change: "create",
        source: command.command === "create_git_service" ? "git" : command.image === null ? "empty" : "image",
      };
      views<ServicesView>("services", command.environment, (view) => ({ ...view, services: [...view.services, service] }));
      return;
    }
    case "create_volume": {
      const volume: VolumeListing = { id: command.id, name: command.name, storage: command.storage,
        storage_locked: false, mounts: [], deployed: false, change: "create" };
      views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: [...view.volumes, volume] }));
      return;
    }
    case "remove_service":
      views<ServicesView>("services", command.environment, (view) => ({ ...view, services: view.services.flatMap((service) =>
        service.name !== command.service ? [service] : service.change === "create" ? [] : [{ ...service, change: "delete" as const }]) }));
      return;
    case "set_volume_storage":
      views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: view.volumes.map((volume) =>
        volume.name === command.volume ? { ...volume, storage: command.storage } : volume) }));
      return;
    case "remove_volume":
      views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: view.volumes.flatMap((volume) =>
        volume.name !== command.volume ? [volume] : volume.change === "create" ? [] : [{ ...volume, change: "delete" as const }]) }));
      return;
    case "rename_service": {
      const from = `${command.service}.`;
      const renamed = (path: string) => path.startsWith(from) ? `${command.name}.${path.slice(from.length)}` : path;
      views<ServicesView>("services", command.environment, (view) => ({ ...view, services: view.services.map((service) =>
        service.name === command.service ? { ...service, name: command.name } : service) }));
      views<EnvironmentView>("environment", command.environment, (view) => ({ ...view,
        settings: view.settings.map((row) => ({ ...row, path: renamed(row.path) })) }));
      views<DomainsView>("domains", command.environment, (view) => ({ ...view, domains: view.domains.map((domain) =>
        domain.service === command.service ? { ...domain, service: command.name } : domain) }));
      return;
    }
    case "rename_volume": {
      // Its mounts are keyed by its name: `SERVICE.mounts.NAME`.
      const mount = `.mounts.${command.volume}`;
      views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: view.volumes.map((volume) =>
        volume.name === command.volume ? { ...volume, name: command.name } : volume) }));
      views<EnvironmentView>("environment", command.environment, (view) => ({ ...view, settings: view.settings.map((row) =>
        row.path.endsWith(mount) ? { ...row, path: `${row.path.slice(0, -mount.length)}.mounts.${command.name}` } : row) }));
      return;
    }
    case "publish":
      views<DiffView>("diff", command.environment, (view) => ({ ...view, published: true }));
      return;
    case "discard": {
      const { path } = command;
      const [node, ...setting] = path?.split(".") ?? [];
      // What goes from the review: every node, one node (a Volume as `volumes.NAME`), or one Setting of one node.
      const volume = node === "volumes" && setting.length === 1 ? setting[0] : null;
      const whole = (change: NodeChange) => path === null
        || (volume !== null ? change.type === "volume" && change.name === volume : setting.length === 0 && change.name === node);
      let reverted: NodeChange["settings"] = [];
      let dropped = new Set<string>();
      views<DiffView>("diff", command.environment, (view) => {
        let removed = 0;
        const changes = view.changes.flatMap((change) => {
          if (whole(change)) {
            reverted = [...reverted, ...change.settings];
            dropped = new Set([...dropped, `${change.type}:${change.name}`]);
            removed += change.settings.length + (change.lifecycle === "update" ? 0 : 1);
            return [];
          }
          const rows = change.settings.filter((row) => row.path !== path && !row.path.startsWith(`${path}.`));
          reverted = [...reverted, ...change.settings.filter((row) => !rows.includes(row))];
          removed += change.settings.length - rows.length;
          if (rows.length === 0 && change.lifecycle === "update") return [];
          return [{ ...change, settings: rows }];
        });
        return { ...view, changes, total_count: path === null ? 0 : Math.max(0, view.total_count - removed), published: true };
      });
      views<EnvironmentView>("environment", command.environment, (view) => ({ ...view, settings: view.settings.map((row) => {
        const deployed = reverted.find((change) => change.path === row.path);
        return deployed ? { ...row, value: deployed.before } : row;
      }) }));
      const of = (type: NodeChange["type"]) => new Set([...dropped].flatMap((key) => key.startsWith(`${type}:`) ? [key.slice(type.length + 1)] : []));
      views<ServicesView>("services", command.environment, (view) => ({ ...view, services: unstage(view.services, of("service")) }));
      views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: unstage(view.volumes, of("volume")) }));
      return;
    }
    case "keep_branch":
      views<BranchView>("branch", command.environment, (view) => ({ ...view, kept: command.kept }));
      return;
    case "set_branch_setup":
      views<EnvironmentsView>("environments", null, (view) => view.project.name !== command.environment.project ? view : {
        ...view, environments: view.environments.map((environment) => environment.name === command.environment.environment
          ? { ...environment, branch_setup: command.setup } : environment),
      });
      return;
    case "set_default_environment":
      views<EnvironmentsView>("environments", null, (view) => view.project.name !== command.environment.project ? view : {
        ...view, environments: view.environments.map((environment) => ({ ...environment, default: environment.name === command.environment.environment })),
      });
      return;
    case "add_domain": {
      // A custom domain names its host; a generated one is the Service's Private DNS under the Cluster Domain.
      const service = command.service;
      // Staged, as the Store words it: live after the next Deploy.
      const staged = { status: "setting_up", reason: "Live after your next deploy", action: { type: "deploy" }, service, port: command.port } as const;
      const domain: DomainsView["domains"][number] = command.hostname === null
        ? { kind: "generated", prefix: service, hostname: null, ...staged }
        : { kind: "custom", hostname: command.hostname, ...staged };
      views<DomainsView>("domains", command.environment, (view) => ({ ...view, domains: [
        ...view.domains.filter((other) => other.service !== service || other.kind !== domain.kind
          || (other.kind === "custom" ? other.hostname !== command.hostname : false)),
        domain,
      ] }));
      // Its pink: the Setting row the Store will list for it (`domainChanged` reads these paths).
      const path = command.hostname === null ? `${service}.managedHostnames` : `${service}.routes.${command.hostname}`;
      const row = { path, kind: "add", before: null, after: command.hostname === null ? service : { hostname: command.hostname }, canRestore: false } as const;
      views<DiffView>("diff", command.environment, (view) => {
        const node = view.changes.find((change) => change.type === "service" && change.name === service);
        const id = node?.id ?? cached("services", command.environment).flatMap((query) => {
          // SAFETY: `cached` found only services views.
          const data = query.state.data as StoreResult<ServicesView> | undefined;
          return data?.ok ? data.value.services.filter((listed) => listed.name === service).map((listed) => listed.id) : [];
        })[0];
        if (id === undefined) return view;
        const changes: NodeChange[] = node
          ? view.changes.map((change) => change === node ? { ...change, settings: [...change.settings.filter((other) => other.path !== path), row] } : change)
          : [...view.changes, { name: service, id, type: "service", lifecycle: "update", comparison: null, data: null, settings: [row] }];
        return { ...view, changes, total_count: view.total_count + 1, published: false };
      });
      return;
    }
    case "remove_domain":
      views<DomainsView>("domains", command.environment, (view) => ({ ...view, domains: view.domains.filter((domain) =>
        domain.kind === "custom" ? domain.hostname !== command.domain : domain.prefix !== command.domain && domain.hostname !== command.domain) }));
      return;
    case "cancel":
      views<DeploymentView>("deployment", null, (view) => view.id !== command.deployment || (view.status !== "queued" && view.status !== "running")
        ? view : { ...view, status: "cancelling" });
      return;
    default:
      return;
  }
}
