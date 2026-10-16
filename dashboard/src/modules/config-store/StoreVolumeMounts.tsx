import { useRef, useState } from "react";
import { BoxIcon, MoreVerticalIcon } from "lucide-react";
import type { DiffView, EnvironmentRef, ServiceListing, VolumeListing } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { FieldError } from "#/components/ui/field";
import { RowWarning, SHARED_VOLUME_WHY } from "#/components/settings/row-warning";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsAddButton, SettingsFoot, SettingsRow, SettingsSection, SettingsWorkspace } from "#/components/settings/settings-section";
import { MountEditor, mountFailure, type MountEditing } from "./MountEditor";
import { detachedMounts, mountChange } from "./store-volumes";
import { mountRefusal, volumeWriters, writersText } from "./volume-sharing";
import { useStoreWriter } from "./store-write";

/** `volumes` is the effective mount projection shared with Scale and the canvas. */
export function StoreVolumeMounts({ resourceId, organizationSlug, environment, services, volumes, diff, replicasOf }: {
  resourceId: string; organizationSlug: string; environment: EnvironmentRef; services: readonly ServiceListing[];
  volumes: readonly VolumeListing[]; diff: DiffView; replicasOf: (service: string) => number;
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
  const fixedVolume = volumes.find((volume) => volume.id === resourceId);
  const ownerActive = fixedVolume !== undefined && fixedVolume.change !== "delete";
  const rows = fixedVolume?.mounts.flatMap((mount) => {
    const service = services.find((one) => one.name === mount.service);
    return service ? [{ service, directory: mount.path, counterpart: service.id }] : [];
  }) ?? [];
  const detached = fixedVolume ? detachedMounts(diff, fixedVolume.name) : [];
  const choices = services.filter((service) => service.change !== "delete").map((service) => ({ id: service.id, name: service.name,
    refusal: fixedVolume ? mountRefusal(fixedVolume, service.name, replicasOf(service.name)) : "Volume missing", directory: "/data" }));
  const available = choices.filter((choice) => !rows.some((row) => row.counterpart === choice.id));

  function resolve(id: string) {
    const volume = fixedVolume;
    const service = services.find((one) => one.id === id);
    if (!volume || volume.change === "delete") throw new Error("This volume is missing or being removed. Choose another volume.");
    if (!service || service.change === "delete") throw new Error("This service is missing or being removed. Choose another service.");
    return { volume, service };
  }

  function detach(id: string) {
    if (pending.current.has(id)) return;
    try {
      const { volume, service } = resolve(id);
      pending.current.add(id); setUnmount({ pending: new Set(pending.current), error: null });
      writer.edit({ environment, changes: [mountChange(service.name, volume.name, null)] }).isPersisted.promise.then(() => {
        pending.current.delete(id); setUnmount((current) => ({ ...current, pending: new Set(pending.current) }));
      }, (error: Error) => {
        pending.current.delete(id); setUnmount({ pending: new Set(pending.current), error: mountFailure(error) });
      });
    } catch (error) {
      pending.current.delete(id); setUnmount({ pending: new Set(pending.current), error: error instanceof Error ? error.message : "Couldn't save the mount. Try again." });
    }
  }

  function warning(volume: VolumeListing) {
    const { shared, writers, total } = volumeWriters(volume, replicasOf);
    if (!shared) return null;
    const correction = writers.length === 1 ? writers[0]?.service : null;
    return <SettingsRow label="Several writers" commit="none"><RowWarning why={SHARED_VOLUME_WHY} action={correction ? (
      <Button type="button" variant="outline" size="xs" onClick={() => writer.edit({ environment, changes: [{ op: "set", path: `${correction}.replicas`, value: 1 }] })}>Use 1 replica</Button>
    ) : null}>
      {total} containers write here: {writersText(writers)}. Can corrupt data.
    </RowWarning></SettingsRow>;
  }

  const reason = !ownerActive ? "This resource is missing or being removed." : choices.length ? "Every service mounts it." : "Add a service to mount this volume.";
  return (
    <SettingsSection id="mounts" title="Mounts" description={!editor && !(ownerActive && available.length) ? reason : undefined}>
      {fixedVolume ? warning(fixedVolume) : null}
      {unmount.pending.size ? <SettingsRow label={<span role="status">Saving unmount…</span>} commit="none" /> : null}
      {unmount.error ? <SettingsRow label="Couldn't unmount" commit="none"><FieldError role="alert">{unmount.error}</FieldError></SettingsRow> : null}
      {rows.length || detached.length ? <SettingsList label="Mounts">
        {rows.map(({ service, directory, counterpart }) => (
          <SettingsListItem key={counterpart} media={<BoxIcon />} label={service.name} commit="change"
            hint={<><span className="break-all font-mono">{directory}</span>{replicasOf(service.name) > 1 ? ` · ${replicasOf(service.name)} replicas` : null}</>}
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
        {detached.map((mount) => <SettingsListItem key={mount.service} media={<BoxIcon />} label={mount.service} tone="removed" commit="none"
          hint={<>Unmounts from <span className="break-all font-mono">{mount.path}</span> on your next deploy. The data stays in this volume.</>} />)}
      </SettingsList> : <SettingsEmpty title="No mounts" />}
      {editor ? (
        <SettingsWorkspace label={<span ref={anchor}>{editor.editing ? "Edit directory" : "Mount on a service"}</span>}>
          <MountEditor key={`${editor.editing}:${editor.counterpart}`}
            choices={choices.filter((choice) => choice.id === editor.counterpart || available.some((one) => one.id === choice.id))} initial={editor}
            onClose={closeEditor} submit={(id, directory) => {
              const { volume, service } = resolve(id);
              const current = volume.mounts.find((mount) => mount.service === service.name);
              if (editor.editing && !current) throw new Error("This mount was removed. Cancel and mount it again.");
              if (!editor.editing && current) throw new Error("This volume is already mounted on that service. Edit its directory instead.");
              if (current?.path === directory) return null;
              const refusal = mountRefusal(volume, service.name, replicasOf(service.name));
              if (refusal) throw new Error(refusal);
              if (volumes.some((other) => other.id !== volume.id && other.mounts.some((mount) => mount.service === service.name && mount.path === directory))) {
                throw new Error(`Another volume is already mounted at ${directory}.`);
              }
              return writer.edit({ environment, changes: [mountChange(service.name, volume.name, directory)] });
            }} />
        </SettingsWorkspace>
      ) : ownerActive && available.length ? (
        <SettingsFoot>
          <SettingsAddButton onClick={() => setEditor({ trigger: null, counterpart: "", directory: "/data", editing: false })}>Mount on a service</SettingsAddButton>
        </SettingsFoot>
      ) : null}
    </SettingsSection>
  );
}
