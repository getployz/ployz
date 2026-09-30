import type { QueryClient } from "@tanstack/react-query";
import type {
  BranchView, BuildOrderView, ConfigCommand, ConfigQuery, DeploymentView, DiffView, DomainsView, EnvironmentRef, EnvironmentsView,
  EnvironmentView, NodeChange, PrPlansView, ServiceListing, ServicesView, VolumeListing, VolumesView,
} from "@ployz/sdk";
import type { StoreResult } from "./store.contract";
import { environmentKey, queryOf, storeViewPrefix } from "./store-view.queries";

/**
 * Shows a Store command in the cached views at once, as the Store will answer once it commits: what the user sees while
 * it saves. The writer's refetch after the commit (or its refusal, which is the rollback) replaces the guess. It guesses
 * only what the command says outright; anything the Store derives (a rename's diff rows, a Move's changes) waits.
 */
// ponytail: Discard shows the deployed values (`before`), which is what it restores unless changes were published first.
export async function applyOptimistic(queryClient: QueryClient, organizationSlug: string, command: ConfigCommand) {
  const cached = (kind: ConfigQuery["query"], environment: EnvironmentRef | null) =>
    queryClient.getQueryCache().findAll({ queryKey: storeViewPrefix(organizationSlug) }).filter((query) => {
      const config = queryOf(query);
      return config?.query === kind
        && (!environment || ("environment" in config && environmentKey(config.environment) === environmentKey(environment)));
    });
  // A read already in flight answers from before the command: it's cancelled first, so it can't land over the guess.
  const views = async <V,>(kind: ConfigQuery["query"], environment: EnvironmentRef | null, update: (view: V) => V) => {
    for (const query of cached(kind, environment)) {
      await queryClient.cancelQueries({ queryKey: query.queryKey, exact: true });
      queryClient.setQueryData<StoreResult<V>>(query.queryKey, (old) => old?.ok ? { ok: true, value: update(old.value) } : old);
    }
  };
  // The Services as the tab last read them.
  const listed = (environment: EnvironmentRef) => cached("services", environment).flatMap((query) => {
    // SAFETY: `cached` found only services views.
    const data = query.state.data as StoreResult<ServicesView> | undefined;
    return data?.ok ? data.value.services : [];
  });
  // A node gone from the review is back to how it is deployed: a new one goes, a removal stays.
  const unstage = <L extends { name: string; change: ServiceListing["change"] }>(listed: L[], names: ReadonlySet<string>) =>
    listed.flatMap((node) => !names.has(node.name) ? [node] : node.change === "create" ? [] : [{ ...node, change: null }]);

  switch (command.command) {
    case "create_service":
    case "create_git_service": {
      const service: ServiceListing = {
        id: command.id, name: command.name, private_dns: command.name, change: "create",
        source: command.command === "create_git_service" ? "git" : command.image === null ? "empty" : "image",
      };
      await views<ServicesView>("services", command.environment, (view) => ({ ...view, services: [...view.services, service] }));
      return;
    }
    case "create_volume": {
      const volume: VolumeListing = { id: command.id, name: command.name, storage: command.storage,
        storage_locked: false, mounts: [], deployed: false, change: "create" };
      await views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: [...view.volumes, volume] }));
      return;
    }
    case "remove_service":
      await views<ServicesView>("services", command.environment, (view) => ({ ...view, services: view.services.flatMap((service) =>
        service.name !== command.service ? [service] : service.change === "create" ? [] : [{ ...service, change: "delete" as const }]) }));
      return;
    case "set_volume_storage":
      await views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: view.volumes.map((volume) =>
        volume.name === command.volume ? { ...volume, storage: command.storage } : volume) }));
      return;
    case "remove_volume":
      await views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: view.volumes.flatMap((volume) =>
        volume.name !== command.volume ? [volume] : volume.change === "create" ? [] : [{ ...volume, change: "delete" as const }]) }));
      return;
    case "rename_service": {
      const from = `${command.service}.`;
      const renamed = (path: string) => path.startsWith(from) ? `${command.name}.${path.slice(from.length)}` : path;
      await views<ServicesView>("services", command.environment, (view) => ({ ...view, services: view.services.map((service) =>
        service.name === command.service ? { ...service, name: command.name } : service) }));
      await views<EnvironmentView>("environment", command.environment, (view) => ({ ...view,
        settings: view.settings.map((row) => ({ ...row, path: renamed(row.path) })) }));
      await views<DomainsView>("domains", command.environment, (view) => ({ ...view, domains: view.domains.map((domain) =>
        domain.service === command.service ? { ...domain, service: command.name } : domain) }));
      return;
    }
    case "rename_volume": {
      // Its mounts are keyed by its name: `SERVICE.mounts.NAME`.
      const mount = `.mounts.${command.volume}`;
      await views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: view.volumes.map((volume) =>
        volume.name === command.volume ? { ...volume, name: command.name } : volume) }));
      await views<EnvironmentView>("environment", command.environment, (view) => ({ ...view, settings: view.settings.map((row) =>
        row.path.endsWith(mount) ? { ...row, path: `${row.path.slice(0, -mount.length)}.mounts.${command.name}` } : row) }));
      return;
    }
    case "publish":
      await views<DiffView>("diff", command.environment, (view) => ({ ...view, published: true }));
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
      // The discarded rows go. The count drops by them, to none once nothing remains; the Store's answer settles it.
      await views<DiffView>("diff", command.environment, (view) => {
        let removed = 0;
        const changes = view.changes.flatMap((change) => {
          if (whole(change)) {
            reverted = [...reverted, ...change.settings];
            dropped = new Set([...dropped, `${change.type}:${change.name}`]);
            return [];
          }
          const rows = change.settings.filter((row) => row.path !== path && !row.path.startsWith(`${path}.`));
          reverted = [...reverted, ...change.settings.filter((row) => !rows.includes(row))];
          removed += change.settings.length - rows.length;
          return rows.length === 0 && change.lifecycle === "update" ? [] : [{ ...change, settings: rows }];
        });
        // Whether what remains is published is the Store's to say.
        return { ...view, changes, total_count: changes.length === 0 ? 0 : Math.max(0, view.total_count - removed) };
      });
      await views<EnvironmentView>("environment", command.environment, (view) => ({ ...view, settings: view.settings.map((row) => {
        const deployed = reverted.find((change) => change.path === row.path);
        return deployed ? { ...row, value: deployed.before } : row;
      }) }));
      const of = (type: NodeChange["type"]) => new Set([...dropped].flatMap((key) => key.startsWith(`${type}:`) ? [key.slice(type.length + 1)] : []));
      await views<ServicesView>("services", command.environment, (view) => ({ ...view, services: unstage(view.services, of("service")) }));
      await views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: unstage(view.volumes, of("volume")) }));
      return;
    }
    case "keep_branch":
      await views<BranchView>("branch", command.environment, (view) => ({ ...view, kept: command.kept }));
      return;
    case "set_branch_setup":
      await views<EnvironmentsView>("environments", null, (view) => view.project.name !== command.environment.project ? view : {
        ...view, environments: view.environments.map((environment) => environment.name === command.environment.environment
          ? { ...environment, branch_setup: command.setup } : environment),
      });
      return;
    case "set_default_environment":
      await views<EnvironmentsView>("environments", null, (view) => view.project.name !== command.environment.project ? view : {
        ...view, environments: view.environments.map((environment) => ({ ...environment, default: environment.name === command.environment.environment })),
      });
      return;
    case "add_domain": {
      // A custom domain names its host; a generated one is the Service's Private DNS under the Cluster Domain.
      const service = command.service;
      const known = listed(command.environment).find((listing) => listing.name === service);
      // Staged, as the Store words it: live after the next Deploy.
      const staged = { status: "setting_up", reason: "Live after your next deploy", action: { type: "deploy" }, service, port: command.port } as const;
      const domain: DomainsView["domains"][number] = command.hostname === null
        ? { kind: "generated", prefix: known?.private_dns ?? service, hostname: null, ...staged }
        : { kind: "custom", hostname: command.hostname, ...staged };
      await views<DomainsView>("domains", command.environment, (view) => ({ ...view, domains: [
        ...view.domains.filter((other) => other.service !== service || other.kind !== domain.kind
          || (other.kind === "custom" ? other.hostname !== command.hostname : false)),
        domain,
      ] }));
      // Its pink: the Setting row the Store will list for it (`domainChanged` reads these paths).
      const path = command.hostname === null ? `${service}.managedHostnames` : `${service}.routes.${command.hostname}`;
      const row = { path, kind: "add", before: null, after: command.hostname === null ? service : { hostname: command.hostname }, canRestore: false } as const;
      await views<DiffView>("diff", command.environment, (view) => {
        const node = view.changes.find((change) => change.type === "service" && change.name === service);
        const id = node?.id ?? known?.id;
        if (id === undefined) return view;
        const changes: NodeChange[] = node
          ? view.changes.map((change) => change === node ? { ...change, settings: [...change.settings.filter((other) => other.path !== path), row] } : change)
          : [...view.changes, { name: service, id, type: "service", lifecycle: "update", comparison: null, data: null, settings: [row] }];
        return { ...view, changes, total_count: view.total_count + 1, published: false };
      });
      return;
    }
    case "remove_domain":
      await views<DomainsView>("domains", command.environment, (view) => ({ ...view, domains: view.domains.filter((domain) =>
        domain.kind === "custom" ? domain.hostname !== command.domain : domain.prefix !== command.domain && domain.hostname !== command.domain) }));
      return;
    case "cancel":
      // A queued one is cancelled at once; a running one is cancelling until its runner stops.
      await views<DeploymentView>("deployment", null, (view) => view.id !== command.deployment ? view
        : view.status === "queued" ? { ...view, status: "cancelled" } : view.status === "running" ? { ...view, status: "cancelling" } : view);
      return;
    case "set_pr_plan": {
      // Each field the command leaves null stays as it is.
      const { project, repository, enabled, start_from: startFrom, copy, setup, remove_on_close: removeOnClose, include_bots: includeBots } = command;
      await views<PrPlansView>("pr_plans", null, (view) => project !== null && view.project.name !== project ? view : {
        ...view, plans: view.plans.map((plan) => plan.repository !== repository ? plan : {
          ...plan, enabled: enabled ?? plan.enabled, start_from: startFrom ?? plan.start_from, copy: copy ?? plan.copy,
          setup: setup ?? plan.setup, remove_on_close: removeOnClose ?? plan.remove_on_close, include_bots: includeBots ?? plan.include_bots,
        }),
      });
      return;
    }
    case "set_build_order":
      await views<BuildOrderView>("build_order", null, (view) => ({ ...view, build_order: command.build_order }));
      return;
    default:
      return;
  }
}
