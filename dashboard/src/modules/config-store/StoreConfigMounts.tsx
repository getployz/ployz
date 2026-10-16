import { useRef, useState } from "react";
import { BoxIcon, MoreVerticalIcon } from "lucide-react";
import type { ConfigListing, DiffView, EnvironmentRef, ServiceListing } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { FieldError } from "#/components/ui/field";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsAddButton, SettingsFoot, SettingsRow, SettingsSection, SettingsWorkspace } from "#/components/settings/settings-section";
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
  const anchor = useRef<HTMLSpanElement | null>(null);
  function closeEditor() {
    // The Add button unmounts while its form is open, so focus falls back to the one the foot renders again.
    const card = anchor.current?.closest("[data-slot=settings-card]");
    setEditor(null);
    requestAnimationFrame(() => {
      const target = editor?.trigger?.isConnected ? editor.trigger
        : card?.querySelector<HTMLButtonElement>("[data-archetype=list-foot] button") ?? card?.querySelector<HTMLButtonElement>("button");
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

  const reason = !ownerActive ? "This resource is missing or being removed." : choices.length ? "Every service mounts it." : "Add a service to mount this config.";
  const directoryDefault = `/etc/${fixedConfig?.name ?? "app"}`;
  return (
    <SettingsSection id="mounts" title="Mounts" description={!editor && !(ownerActive && available.length) ? reason : undefined}>
      {unmount.pending.size ? <SettingsRow label={<span role="status">Saving unmount…</span>} commit="none" /> : null}
      {unmount.error ? <SettingsRow label="Couldn't unmount" commit="none"><FieldError role="alert">{unmount.error}</FieldError></SettingsRow> : null}
      {rows.length || detached.length ? <SettingsList label="Mounts">
        {rows.map(({ service, directory, counterpart }) => (
          <SettingsListItem key={counterpart} media={<BoxIcon />} label={service.name} commit="change"
            hint={<><span className="break-all font-mono">{directory}</span> · Read-only</>}
            actions={(
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
            )} />
        ))}
        {detached.map((mount) => <SettingsListItem key={mount.serviceId} media={<BoxIcon />} label={mount.service} tone="removed" commit="none"
          hint={<>Unmounts from <span className="break-all font-mono">{mount.directory}</span> on your next deploy. The config and its files stay.</>} />)}
      </SettingsList> : <SettingsEmpty title="No mounts" />}
      {editor ? (
        <SettingsWorkspace label={<span ref={anchor}>{editor.editing ? "Edit directory" : "Mount on a service"}</span>}>
          <MountEditor key={`${editor.editing}:${editor.counterpart}`}
            choices={choices.filter((choice) => choice.id === editor.counterpart || available.some((one) => one.id === choice.id))} initial={editor}
            onClose={closeEditor} submit={(id, directory) => {
              const { config, service } = resolve(id);
              const current = config.mounts.find((mount) => mount.service === service.name);
              if (editor.editing && !current) throw new Error("This mount was removed. Cancel and mount it again.");
              if (!editor.editing && current) throw new Error("This config is already mounted on that service. Edit its directory instead.");
              if (current?.dir === directory) return null;
              return writer.commit(attachConfigCommand(environment, service.name, config.name, directory), ["invalid", "conflict"]);
            }} />
        </SettingsWorkspace>
      ) : ownerActive && available.length ? (
        <SettingsFoot>
          <SettingsAddButton onClick={() => setEditor({ trigger: null, counterpart: "", directory: directoryDefault, editing: false })}>Mount on a service</SettingsAddButton>
        </SettingsFoot>
      ) : null}
    </SettingsSection>
  );
}
