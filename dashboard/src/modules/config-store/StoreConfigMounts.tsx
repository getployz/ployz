import { useRef, useState } from "react";
import { MoreVerticalIcon } from "lucide-react";
import type { ConfigListing, DiffView, EnvironmentRef, ServiceListing } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { FieldError } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { MountEditor, mountFailure, type MountEditing } from "./MountEditor";
import { attachConfigCommand, detachedConfigMounts } from "./store-configs";
import { useStoreWriter } from "./store-write";

export function StoreConfigMounts({ resourceId, organizationSlug, environment, services, configs, diff }: {
  resourceId: string; organizationSlug: string; environment: EnvironmentRef; services: readonly ServiceListing[];
  configs: readonly ConfigListing[]; diff: DiffView;
}) {
  const writer = useStoreWriter(organizationSlug);
  const [editor, setEditor] = useState<(MountEditing & { trigger: HTMLButtonElement | null }) | null>(null);
  const [unmount, setUnmount] = useState<{ pending: ReadonlySet<string>; error: string | null }>({ pending: new Set(), error: null });
  const pending = useRef(new Set<string>());
  const trigger = useRef<HTMLButtonElement | null>(null);
  const panel = useRef<HTMLDivElement | null>(null);
  function closeEditor() {
    setEditor(null);
    requestAnimationFrame(() => {
      const target = editor?.trigger?.isConnected ? editor.trigger : panel.current?.querySelector<HTMLButtonElement>("button");
      target?.focus();
    });
  }
  const fixedConfig = configs.find((config) => config.id === resourceId);
  const ownerActive = fixedConfig !== undefined && fixedConfig.change !== "delete";
  const rows = fixedConfig?.mounts.flatMap((mount) => {
    const service = services.find((one) => one.name === mount.service);
    return service ? [{ service, directory: mount.dir, counterpart: service.id }] : [];
  }) ?? [];
  const detached = fixedConfig ? detachedConfigMounts(diff, fixedConfig.id) : [];
  const choices = services.filter((service) => service.change !== "delete")
    .map((service) => ({ id: service.id, name: service.name, refusal: null, directory: `/etc/${fixedConfig?.name ?? "app"}` }));
  const available = choices.filter((choice) => !rows.some((row) => row.counterpart === choice.id));

  function resolve(id: string) {
    const config = fixedConfig;
    const service = services.find((one) => one.id === id);
    if (!config || config.change === "delete") throw new Error("This config is missing or being removed. Choose another config.");
    if (!service || service.change === "delete") throw new Error("This service is missing or being removed. Choose another service.");
    return { config, service };
  }

  function detach(id: string) {
    if (pending.current.has(id)) return;
    try {
      const { config, service } = resolve(id);
      pending.current.add(id); setUnmount({ pending: new Set(pending.current), error: null });
      writer.commit({ command: "detach_config", environment, service: service.name, config: config.name }, ["invalid", "conflict"])
        .isPersisted.promise.then(() => {
          pending.current.delete(id); setUnmount((current) => ({ ...current, pending: new Set(pending.current) }));
        }, (error: Error) => {
          pending.current.delete(id); setUnmount({ pending: new Set(pending.current), error: mountFailure(error) });
        });
    } catch (error) {
      pending.current.delete(id); setUnmount({ pending: new Set(pending.current), error: error instanceof Error ? error.message : "Couldn't save the mount. Try again." });
    }
  }

  return (
    <SettingsSection id="mounts" title="Mounts" action={
      ownerActive && available.length ? <Button variant="outline" size="sm" disabled={editor !== null}
        onClick={(event) => { setEditor({ trigger: event.currentTarget, counterpart: "", directory: `/etc/${fixedConfig?.name ?? "app"}`, editing: false }); }}>Mount on a service</Button> : null
    }>
      <div ref={panel} className="flex flex-col gap-3">
        {unmount.error ? <FieldError role="alert">{unmount.error}</FieldError> : null}
        {unmount.pending.size ? <p role="status" className="text-sm text-muted-foreground">Saving unmount…</p> : null}
        {rows.length ? <p className="text-sm text-muted-foreground">Read-only mounts</p> : null}
        {editor ? <MountEditor key={`${editor.editing}:${editor.counterpart}`}
          choices={choices.filter((choice) => choice.id === editor.counterpart || available.some((one) => one.id === choice.id))} initial={editor}
          onClose={closeEditor} submit={(id, directory) => {
            const { config, service } = resolve(id);
            const current = config.mounts.find((mount) => mount.service === service.name);
            if (editor.editing && !current) throw new Error("This mount was removed. Cancel and mount it again.");
            if (!editor.editing && current) throw new Error("This config is already mounted on that service. Edit its directory instead.");
            if (current?.dir === directory) return null;
            return writer.commit(attachConfigCommand(environment, service.name, config.name, directory), ["invalid", "conflict"]);
          }} /> : null}
        {rows.map(({ service, directory, counterpart }) => (
          <Item key={counterpart} size="xs">
            <ItemContent className="min-w-0">
              <ItemTitle className="line-clamp-none flex-wrap">{service.name} <span className="break-all font-mono font-normal text-muted-foreground">{directory}</span></ItemTitle>
            </ItemContent>
            <ItemActions>
              <DropdownMenu>
                <DropdownMenuTrigger disabled={unmount.pending.has(counterpart)}
                  onFocus={(event) => { trigger.current = event.currentTarget; }}
                  onClick={(event) => { trigger.current = event.currentTarget; }}
                  render={<Button variant="ghost" size="icon-sm" aria-label={`Actions for ${service.name} mount`} />}>
                  <MoreVerticalIcon />
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-auto" finalFocus={editor ? false : undefined}>
                  <DropdownMenuItem disabled={unmount.pending.has(counterpart)} onClick={() => setEditor({ trigger: trigger.current, counterpart, directory, editing: true })}>Edit directory</DropdownMenuItem>
                  <DropdownMenuItem disabled={unmount.pending.has(counterpart)} onClick={() => detach(counterpart)}>Unmount</DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </ItemActions>
          </Item>
        ))}
        {detached.map((mount) => <Item key={mount.serviceId} variant="muted" size="xs"><ItemContent>
          <ItemTitle>{mount.service}</ItemTitle>
          <ItemDescription>Unmounts from <span className="break-all font-mono">{mount.directory}</span> on your next deploy. The config and its files stay.</ItemDescription>
        </ItemContent></Item>)}
        {!editor && !(ownerActive && available.length) ? <p className="text-sm text-muted-foreground">{!ownerActive ? "This resource is missing or being removed." : choices.length ? "Every service mounts it." : "Add a service to mount this config."}</p> : null}
      </div>
    </SettingsSection>
  );
}
