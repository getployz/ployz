import type { QueryClient } from "@tanstack/react-query";
import type {
  BranchView, BuildOrderView, ConfigCommand, ConfigQuery, DeploymentView, DiffView, DomainsView, EnvironmentRef, EnvironmentsView,
  EnvironmentView, NodeChange, PrPlansView, ProjectsView, RowId, ServiceListing, ServicesView, SyncView,
  VolumeListing, VolumesView,
} from "@ployz/sdk";
import type { StoreResult } from "./store.contract";
import { environmentKey, queryOf, storeViewPrefix } from "./store-view.queries";

/**
 * Shows a Store command in the cached views at once, as the Store will answer once it commits: what the user sees while
 * it saves. The writer's refetch after the commit (or its refusal, which is the rollback) replaces the guess. It guesses
 * only what the command says outright; anything the Store derives (a rename's diff rows, a Sync's changes) waits.
 */
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
  // A Setting row the Store will list in the review: its pink (`domainChanged` reads these paths).
  const stage = (environment: EnvironmentRef, service: string, row: NodeChange["settings"][number]) =>
    views<DiffView>("diff", environment, (view) => {
      const node = view.changes.find((change) => change.type === "service" && change.name === service);
      const listing = node ? null : listed(environment).find((one) => one.name === service);
      const changes: NodeChange[] | null = node
        ? view.changes.map((change) => change === node ? { ...change, settings: [...change.settings.filter((other) => other.path !== row.path), row] } : change)
        : listing ? [...view.changes, { name: service, id: listing.id, row: listing.row, type: "service", lifecycle: "update", comparison: null, data: null, restarts: [], settings: [row] }] : null;
      if (!changes) return view;
      // The count and whether it's published are the Store's to say: they come with the write's answer.
      return { ...view, changes };
    });
  // The Services as the tab last read them.
  const listed = (environment: EnvironmentRef) => cached("services", environment).flatMap((query) => {
    // SAFETY: `cached` found only services views.
    const data = query.state.data as StoreResult<ServicesView> | undefined;
    return data?.ok ? data.value.services : [];
  });

  switch (command.command) {
    case "batch":
      for (const inner of command.commands) await applyOptimistic(queryClient, organizationSlug, inner);
      return;
    case "create_service":
    case "create_git_service": {
      // SAFETY: a new Service's id is its lineage, and a node's RowId is `{lineage}:node`.
      const service: ServiceListing = {
        id: command.id, row: `${command.id}:node` as RowId, name: command.name, private_dns: command.name, change: "create",
        source: command.command === "create_git_service" ? "git" : command.image === null ? "empty" : "image",
        template: command.command === "create_service" ? command.template ?? null : null,
      };
      await views<ServicesView>("services", command.environment, (view) => ({ ...view, services: [...view.services, service] }));
      return;
    }
    case "create_volume": {
      const volume: VolumeListing = { id: command.id, name: command.name, storage: command.storage,
        storage_locked: false, shared_writes: command.shared_writes ?? false, mounts: [], deployed: false, change: "create" };
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
    case "set_volume_shared_writes":
      // At once, not staged: it changes what the Store allows, not what a Deploy does.
      await views<VolumesView>("volumes", command.environment, (view) => ({ ...view, volumes: view.volumes.map((volume) =>
        volume.name === command.volume ? { ...volume, shared_writes: command.shared_writes } : volume) }));
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
      // Its rows leave the review at once; what the Store restores (values, nodes, the count) comes with its answer.
      const { path } = command;
      const [node, ...setting] = path?.split(".") ?? [];
      // Every node, one node (a Volume as `volumes.NAME`), or one Setting of one node.
      const volume = node === "volumes" && setting.length === 1 ? setting[0] : null;
      const whole = (change: NodeChange) => path === null
        || (volume !== null ? change.type === "volume" && change.name === volume : setting.length === 0 && change.name === node);
      await views<DiffView>("diff", command.environment, (view) => {
        const changes = view.changes.flatMap((change) => {
          if (whole(change)) return [];
          const rows = change.settings.filter((row) => row.path !== path && !row.path.startsWith(`${path}.`));
          return rows.length === 0 && change.lifecycle === "update" ? [] : [{ ...change, settings: rows }];
        });
        // An empty review counts nothing.
        return changes.length === 0 ? { ...view, changes, total_count: 0 } : { ...view, changes };
      });
      return;
    }
    case "never_sync": {
      // At once: a mark changes what Sync offers, not Working State.
      const rows = new Set(command.rows);
      await views<EnvironmentView>("environment", command.environment, (view) => {
        const kept = view.never_synced?.filter((row) => !rows.has(row)) ?? [];
        // SAFETY: the dashboard names a row only by the RowId a read gave.
        return { ...view, never_synced: command.off ? kept : [...kept, ...command.rows as RowId[]] };
      });
      // A Sync from or into it offers a newly marked row no more; an unmarked row loses this Environment's mark, and
      // with no mark left, waits for the Store to offer it again.
      // ponytail: a Parent's mark doesn't keep a row from its own direct Branch; that Sync shows it marked until the Store answers.
      const { project, environment: name } = command.environment;
      if (!name) return;
      const here = (side: SyncView["from"]) => side.project === project && side.name === name;
      await views<SyncView>("sync", null, (view) => {
        if (!here(view.from) && !here(view.into)) return view;
        if (command.off) {
          return { ...view, never_synced: view.never_synced.flatMap((entry) => {
            const marks = entry.marks.filter((mark) => mark.environment !== name || !rows.has(mark.row));
            return marks.length ? [{ ...entry, marks }] : [];
          }) };
        }
        const marking = view.rows.filter((row) => rows.has(row.row));
        return {
          ...view,
          rows: view.rows.filter((row) => !rows.has(row.row)),
          never_synced: [
            ...view.never_synced,
            ...marking.map(({ row, node, kind, name: at }) => ({ row, node, kind, name: at, marks: [{ environment: name, row }] })),
          ],
        };
      });
      return;
    }
    case "keep_branch":
      // Kept, it never closes for sitting idle; when it would again is the Store's to say.
      await views<BranchView>("branch", command.environment, (view) => ({ ...view, kept: command.kept, closes_at: command.kept ? null : view.closes_at }));
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
      const path = command.hostname === null ? `${service}.managedHostnames` : `${service}.routes.${command.hostname}`;
      await stage(command.environment, service, { path, kind: "add", before: null, after: command.hostname === null ? service : { hostname: command.hostname }, canRestore: false, row: null });
      return;
    }
    case "set_generated_domain": {
      const { service, prefix, port } = command;
      // The hostname follows the prefix under the same Cluster Domain; an omitted port stays.
      await views<DomainsView>("domains", command.environment, (view) => ({ ...view, domains: view.domains.map((domain) =>
        domain.kind !== "generated" || domain.service !== service ? domain : {
          ...domain, prefix, port: port === undefined ? domain.port : port,
          hostname: domain.hostname === null ? null : `${prefix}${domain.hostname.slice(domain.prefix.length)}`,
        }) }));
      await stage(command.environment, service, { path: `${service}.managedHostnames`, kind: "update", before: null, after: prefix, canRestore: false, row: null });
      return;
    }
    case "rename_project":
      await views<ProjectsView>("projects", null, (view) => ({ ...view, projects: view.projects.map((project) =>
        project.name === command.project ? { ...project, name: command.name } : project) }));
      await views<EnvironmentsView>("environments", null, (view) => view.project.name !== command.project ? view
        : { ...view, project: { ...view.project, name: command.name } });
      return;
    case "remove_domain":
      await views<DomainsView>("domains", command.environment, (view) => ({ ...view, domains: view.domains.filter((domain) =>
        domain.kind === "custom" ? domain.hostname !== command.domain : domain.prefix !== command.domain && domain.hostname !== command.domain) }));
      return;
    case "cancel":
      // Cancelling until the Store answers with where it ended.
      await views<DeploymentView>("deployment", null, (view) => view.id === command.deployment && view.in_flight ? { ...view, status: "cancelling" } : view);
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
