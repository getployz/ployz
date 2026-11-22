import { useRef, useState, type ReactNode } from "react";
import { BoxIcon, MoreVerticalIcon } from "lucide-react";
import type { ConfigListing, DiffView, EnvironmentRef, JsonValue, ServiceListing, VolumeListing } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { FieldError } from "#/components/ui/field";
import { RowWarning } from "#/components/settings/row-warning";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsAddButton, SettingsFoot, SettingsRow, SettingsSection, SettingsWorkspace } from "#/components/settings/settings-section";
import { desiredReplicas, oneReplica } from "#/modules/config-store/replica-rows";
import { attachConfigCommand, detachedConfigMounts } from "#/modules/config-store/store-configs";
import { detachedMounts, mountChange } from "#/modules/config-store/store-volumes";
import { mountRefusal, volumeWriters, writersText } from "#/modules/config-store/volume-sharing";
import { useStoreWriter, type Write } from "#/modules/config-store/store-write";
import { SHARED_VOLUME_WHY } from "#/modules/volume-placement/volume-placement";
import { MountEditor, mountFailure, type MountChoice, type MountEditing } from "./MountEditor";

type MountRow = { counterpart: string; service: ServiceListing; directory: string; hint: ReactNode };
type DetachedRow = { key: string; service: string; hint: ReactNode };

/**
 * A resource's Mounts card, the same for a Volume and a Config: the list, the unmount actions and the mount form.
 * `unmount` and `submit` check the resource and Service afresh, and throw a message to show when one is gone.
 */
function MountsSection({ noun, header, ownerActive, rows, detached, choices, defaultDirectory, unmount: unmountService, submit }: {
  noun: "volume" | "config"; header?: ReactNode; ownerActive: boolean; rows: readonly MountRow[]; detached: readonly DetachedRow[];
  choices: readonly MountChoice[]; defaultDirectory: string;
  unmount: (id: string) => Write;
  submit: (id: string, directory: string, editing: boolean) => Write | null;
}) {
  const [editor, setEditor] = useState<(MountEditing & { trigger: HTMLButtonElement | null }) | null>(null);
  const [unmount, setUnmount] = useState<{ pending: ReadonlySet<string>; error: string | null }>({ pending: new Set(), error: null });
  const pending = useRef(new Set<string>());
  const trigger = useRef<HTMLButtonElement | null>(null);
  const anchor = useRef<HTMLSpanElement | null>(null);
  const available = choices.filter((choice) => !rows.some((row) => row.counterpart === choice.id));

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

  function detach(id: string) {
    if (pending.current.has(id)) return;
    try {
      const write = unmountService(id);
      pending.current.add(id); setUnmount({ pending: new Set(pending.current), error: null });
      write.then(() => {
        pending.current.delete(id); setUnmount((current) => ({ ...current, pending: new Set(pending.current) }));
      }, (error: Error) => {
        pending.current.delete(id); setUnmount({ pending: new Set(pending.current), error: mountFailure(error) });
      });
    } catch (error) {
      pending.current.delete(id); setUnmount({ pending: new Set(pending.current), error: error instanceof Error ? error.message : "Couldn't save the mount. Try again." });
    }
  }

  const reason = !ownerActive ? "This resource is missing or being removed." : choices.length ? "Every service mounts it." : `Add a service to mount this ${noun}.`;
  return (
    <SettingsSection id="mounts" title="Mounts" description={!editor && !(ownerActive && available.length) ? reason : undefined}>
      {header}
      {unmount.pending.size ? <SettingsRow label={<span role="status">Saving unmount…</span>} commit="none" /> : null}
      {unmount.error ? <SettingsRow label="Couldn't unmount" commit="none"><FieldError role="alert">{unmount.error}</FieldError></SettingsRow> : null}
      {rows.length || detached.length ? <SettingsList label="Mounts">
        {rows.map(({ service, directory, counterpart, hint }) => (
          <SettingsListItem key={counterpart} media={<BoxIcon />} label={service.name} commit="change" hint={hint}
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
        {detached.map((mount) => <SettingsListItem key={mount.key} media={<BoxIcon />} label={mount.service} tone="removed" commit="none" hint={mount.hint} />)}
      </SettingsList> : <SettingsEmpty title="No mounts" />}
      {editor ? (
        <SettingsWorkspace label={<span ref={anchor}>{editor.editing ? "Edit directory" : "Mount on a service"}</span>}>
          <MountEditor key={`${editor.editing}:${editor.counterpart}`}
            choices={choices.filter((choice) => choice.id === editor.counterpart || available.some((one) => one.id === choice.id))} initial={editor}
            onClose={closeEditor} submit={(id, directory) => submit(id, directory, editor.editing)} />
        </SettingsWorkspace>
      ) : ownerActive && available.length ? (
        <SettingsFoot>
          <SettingsAddButton onClick={() => setEditor({ trigger: null, counterpart: "", directory: defaultDirectory, editing: false })}>Mount on a service</SettingsAddButton>
        </SettingsFoot>
      ) : null}
    </SettingsSection>
  );
}

const path = (directory: string) => <span className="break-all font-mono">{directory}</span>;

/** `volumes` is the effective mount projection shared with Scale and the canvas. */
export function StoreVolumeMounts({ resourceId, organizationSlug, environment, services, volumes, diff, replicas, split }: {
  resourceId: string; organizationSlug: string; environment: EnvironmentRef; services: readonly ServiceListing[];
  volumes: readonly VolumeListing[]; diff: DiffView;
  /** Each Service's `replicas` as the next Deploy has it. */
  replicas: (service: string) => JsonValue;
  /** Why the Deploy would refuse a Service once it mounts this Volume, for running apart from the Volume's region. */
  split: (service: string) => { short: string; why: string } | null;
}) {
  const writer = useStoreWriter(organizationSlug);
  const replicasOf = (service: string) => desiredReplicas(replicas(service)) ?? 1;
  const fixedVolume = volumes.find((volume) => volume.id === resourceId);
  const ownerActive = fixedVolume !== undefined && fixedVolume.change !== "delete";
  const rows = fixedVolume?.mounts.flatMap((mount) => {
    const service = services.find((one) => one.name === mount.service);
    return service ? [{
      service, directory: mount.path, counterpart: service.id,
      hint: <>{path(mount.path)}{replicasOf(service.name) > 1 ? ` · ${replicasOf(service.name)} replicas` : null}</>,
    }] : [];
  }) ?? [];
  const detached = (fixedVolume ? detachedMounts(diff, fixedVolume.name) : []).map((mount) => ({
    key: mount.service, service: mount.service,
    hint: <>Unmounts from {path(mount.path)} on your next deploy. The data stays in this volume.</>,
  }));
  const choices = services.filter((service) => service.change !== "delete").map((service) => ({ id: service.id, name: service.name,
    refusal: fixedVolume ? whole(mountRefusal(fixedVolume, service.name, replicasOf(service.name))) ?? split(service.name) : whole("Volume missing"), directory: "/data" }));

  function resolve(id: string) {
    const volume = fixedVolume;
    const service = services.find((one) => one.id === id);
    if (!volume || volume.change === "delete") throw new Error("This volume is missing or being removed. Choose another volume.");
    if (!service || service.change === "delete") throw new Error("This service is missing or being removed. Choose another service.");
    return { volume, service };
  }

  function warning(volume: VolumeListing) {
    const { shared, writers, total } = volumeWriters(volume, replicasOf);
    if (!shared) return null;
    const correction = writers.length === 1 ? writers[0]?.service : undefined;
    const one = correction === undefined ? null : oneReplica(replicas(correction));
    return <SettingsRow label="Several writers" commit="none"><RowWarning why={SHARED_VOLUME_WHY} action={correction !== undefined && one !== null ? (
      <Button type="button" variant="outline" size="xs" onClick={() => writer.edit({ environment, changes: [{ op: "set", path: `${correction}.replicas`, value: one }] })}>Use 1 replica</Button>
    ) : null}>
      {total} containers write here: {writersText(writers)}. Can corrupt data.
    </RowWarning></SettingsRow>;
  }

  return (
    <MountsSection noun="volume" header={fixedVolume ? warning(fixedVolume) : null} ownerActive={ownerActive} rows={rows} detached={detached}
      choices={choices} defaultDirectory="/data"
      unmount={(id) => {
        const { volume, service } = resolve(id);
        return writer.edit({ environment, changes: [mountChange(service.name, volume.name, null)] });
      }}
      submit={(id, directory, editing) => {
        const { volume, service } = resolve(id);
        const current = volume.mounts.find((mount) => mount.service === service.name);
        if (editing && !current) throw new Error("This mount was removed. Cancel and mount it again.");
        if (!editing && current) throw new Error("This volume is already mounted on that service. Edit its directory instead.");
        if (current?.path === directory) return null;
        const refusal = mountRefusal(volume, service.name, replicasOf(service.name)) ?? (current ? null : split(service.name)?.why);
        if (refusal) throw new Error(refusal);
        if (volumes.some((other) => other.id !== volume.id && other.mounts.some((mount) => mount.service === service.name && mount.path === directory))) {
          throw new Error(`Another volume is already mounted at ${directory}.`);
        }
        return writer.edit({ environment, changes: [mountChange(service.name, volume.name, directory)] });
      }} />
  );
}

export function StoreConfigMounts({ resourceId, organizationSlug, environment, services, configs, diff }: {
  resourceId: string; organizationSlug: string; environment: EnvironmentRef; services: readonly ServiceListing[];
  configs: readonly ConfigListing[]; diff: DiffView;
}) {
  const writer = useStoreWriter(organizationSlug);
  const fixedConfig = configs.find((config) => config.id === resourceId);
  const ownerActive = fixedConfig !== undefined && fixedConfig.change !== "delete";
  const defaultDirectory = `/etc/${fixedConfig?.name ?? "app"}`;
  const rows = fixedConfig?.mounts.flatMap((mount) => {
    const service = services.find((one) => one.name === mount.service);
    return service ? [{ service, directory: mount.dir, counterpart: service.id, hint: <>{path(mount.dir)} · Read-only</> }] : [];
  }) ?? [];
  const detached = (fixedConfig ? detachedConfigMounts(diff, fixedConfig.id) : []).map((mount) => ({
    key: mount.serviceId, service: mount.service,
    hint: <>Unmounts from {path(mount.directory)} on your next deploy. The config and its files stay.</>,
  }));
  const choices = services.filter((service) => service.change !== "delete")
    .map((service) => ({ id: service.id, name: service.name, refusal: null, directory: defaultDirectory }));

  function resolve(id: string) {
    const config = fixedConfig;
    const service = services.find((one) => one.id === id);
    if (!config || config.change === "delete") throw new Error("This config is missing or being removed. Choose another config.");
    if (!service || service.change === "delete") throw new Error("This service is missing or being removed. Choose another service.");
    return { config, service };
  }

  return (
    <MountsSection noun="config" ownerActive={ownerActive} rows={rows} detached={detached} choices={choices} defaultDirectory={defaultDirectory}
      unmount={(id) => {
        const { config, service } = resolve(id);
        return writer.commit({ command: "detach_config", environment, service: service.name, config: config.name }, ["invalid", "conflict"]);
      }}
      submit={(id, directory, editing) => {
        const { config, service } = resolve(id);
        const current = config.mounts.find((mount) => mount.service === service.name);
        if (editing && !current) throw new Error("This mount was removed. Cancel and mount it again.");
        if (!editing && current) throw new Error("This config is already mounted on that service. Edit its directory instead.");
        if (current?.dir === directory) return null;
        return writer.commit(attachConfigCommand(environment, service.name, config.name, directory), ["invalid", "conflict"]);
      }} />
  );
}

const whole = (why: string | null) => (why === null ? null : { short: why, why });
