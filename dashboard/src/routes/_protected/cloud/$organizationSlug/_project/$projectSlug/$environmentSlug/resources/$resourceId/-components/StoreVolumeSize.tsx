import { useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { EnvironmentRef, VolumeListing } from "@ployz/sdk";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { SettingsRow } from "#/components/settings/settings-section";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { FieldError } from "#/components/ui/field";
import { environmentKey, invalidateViews } from "#/modules/config-store/store-view.queries";
import { gigabytes, volumeStorage } from "#/modules/config-store/store-volumes";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { type Bytes, gb } from "#/modules/volume-run/resize";
import { checkResize, growWords, type Holder, type ResizeVerdict } from "#/modules/volume-run/resize-check";
import { ACTIVE_VOLUME_RUN_STATES, type VolumeRunView } from "#/modules/volume-run/volume-run";
import { runWords } from "#/modules/volume-run/volume-copies";
import { resizeVolumeServerFn } from "#/modules/volume-run/volume-run.functions";
import { useWatchVolumeRun, volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { ServiceSettingInput } from "../../../services/$serviceId/-components/ServiceSettingInput";

type Grow = Extract<ResizeVerdict, { kind: "grow" }>;
type Asking = { grow: Grow; settle: (error?: Error) => void };

const SETTLED: PersistableTransaction = { isPersisted: { promise: Promise.resolve() } };
const active = (run: VolumeRunView) => ACTIVE_VOLUME_RUN_STATES.some((state) => state === run.state);

export function StoreVolumeSize({ organizationSlug, environment, volume, recorded, holders, running }: {
  organizationSlug: string;
  environment: EnvironmentRef;
  volume: VolumeListing;
  recorded: Bytes;
  holders: readonly Holder[] | null;
  running: VolumeRunView | null;
}) {
  const writer = useStoreWriter(organizationSlug);
  const queryClient = useQueryClient();
  const working = volume.storage.kind === "provisioned" ? volume.storage.maximumBytes : recorded;
  const [asking, setAsking] = useState<Asking | null>(null);
  const admittingGrow = useRef(false);
  const [growingTo, setGrowingTo] = useState<Bytes | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const { watch, watching, runs } = useWatchVolumeRun(organizationSlug, (run) => {
    setGrowingTo(null);
    if (run?.state === "done") void invalidateViews(queryClient, organizationSlug, new Set([environmentKey(environment)]));
    else setFailure(run?.message ?? `Could not grow ${volume.name}.`);
  });
  const restoredSmaller = working < recorded;
  const resizing = runs.find(active) ?? (running?.kind === "resize" ? running : null);
  const other = running !== null && running.kind !== "resize" ? running : null;
  const growing = resizing !== null || (watching && growingTo !== null);

  const verdict = (raw: string) => {
    const typed = volumeStorage(true, raw);
    if (typed?.kind !== "provisioned") return null;
    return checkResize({ volume: volume.name, recorded, working, requested: typed.maximumBytes, holders });
  };

  function commit(raw: string): PersistableTransaction {
    setFailure(null);
    const next = verdict(raw);
    if (next?.kind === "restage") {
      return writer.commit({ command: "set_volume_storage", environment, volume: volume.name, storage: { kind: "provisioned", maximumBytes: recorded } });
    }
    if (next?.kind !== "grow") return SETTLED;
    return { isPersisted: { promise: new Promise<void>((resolve, reject) => setAsking({ grow: next, settle: (error) => (error ? reject(error) : resolve()) })) } };
  }

  async function grow(asked: Asking) {
    admittingGrow.current = true;
    try {
      const { id } = await resizeVolumeServerFn({ data: {
        organizationSlug, volumeId: volume.id, environment, maximumBytes: asked.grow.to, requestId: crypto.randomUUID(),
      } });
      setGrowingTo(asked.grow.to);
      watch(id);
      void queryClient.invalidateQueries({ queryKey: volumeRunKeys.all });
      asked.settle();
    } catch (error) {
      asked.settle(error instanceof Error ? error : new Error(`Could not grow ${volume.name}.`));
    } finally {
      admittingGrow.current = false;
    }
    setAsking((current) => (current === asked ? null : current));
  }

  const hint = growing
    ? resizing?.message ?? `${resizing ? runWords(resizing) : `Growing to ${gb(growingTo ?? recorded)}`}…`
    : other !== null
    ? `${runWords(other)}. Grow it once that finishes.`
    : restoredSmaller
    ? `A restored revision asks for ${gb(working)}; the servers keep ${gb(recorded)}. Enter ${gigabytes(recorded)} to keep it.`
    : "Grows live on every server that holds a copy. Can't shrink yet.";

  return (
    <>
      <SettingsRow label="Storage limit" hint={hint} commit="confirm" control={(
        <ServiceSettingInput ariaLabel="Storage limit" inputMode="decimal" suffix="GB" disabled={growing || other !== null}
          value={gigabytes(Math.min(working, recorded))} isChanged={restoredSmaller}
          validate={(raw) => {
            const next = verdict(raw);
            return next === null ? "Enter at least 0.001 GB." : next.kind === "refused" ? next.message : null;
          }}
          onCommit={commit} />
      )}>
        {failure ? <FieldError>{failure}</FieldError> : null}
      </SettingsRow>
      <ConfirmDialog open={asking !== null} title="Grow volume" actionLabel="Grow" pendingLabel="Growing"
        description={asking ? growWords(volume.name, asking.grow) : null}
        onOpenChange={(open) => {
          if (open || asking === null || admittingGrow.current) return;
          asking.settle();
          setAsking(null);
        }}
        onConfirm={() => (asking ? grow(asking) : undefined)} />
    </>
  );
}
