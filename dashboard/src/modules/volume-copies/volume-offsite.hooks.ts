import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { EnvironmentRef } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import type { UploadResult } from "./volume-offsite.server";
import type { BackupChoice } from "./volume-offsite";
import { addVolumeOffsiteServerFn, uploadVolumeOffsiteServerFn } from "./volume-offsite.functions";
import { useVolumeOffsiteRead } from "./volume-offsite.queries";

const ASK_AGAIN_MS = 5_000;

/**
 * The Volume's offsite copy, with Add and Upload now. An upload is long work on the writer, so the hook asks again with
 * the same source until the status leaves running, the way Refresh now watches its round.
 */
export function useVolumeOffsite(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  const queryClient = useQueryClient();
  const read = useVolumeOffsiteRead(organizationSlug, environment, volumeId);
  const [sending, setSending] = useState(false);
  const [watching, setWatching] = useState<string | null>(null);
  const [field, setField] = useState<string | null>(null);

  const settle = (result: UploadResult, done: string) => {
    void queryClient.invalidateQueries({ queryKey: volumeRunKeys.all });
    if (!result.ok) {
      if ("field" in result) setField(result.field);
      else toast.error(result.refusal.message);
      setWatching(null);
      return false;
    }
    setField(null);
    if (result.status.kind === "running" && result.from !== null) {
      setWatching(result.from);
      return true;
    }
    setWatching(null);
    if (result.status.kind === "failed") toast.error(result.status.reason);
    else if (result.from !== null) toast.success(done);
    return true;
  };

  useEffect(() => {
    if (watching === null) return;
    const timer = setTimeout(() => {
      uploadVolumeOffsiteServerFn({ data: { organizationSlug, environment, volumeId, from: watching } })
        .then((result) => settle(result, "Uploaded"), (error: Error) => {
          setWatching(null);
          toast.error(toErrorMessage(error, "Upload failed"));
        });
    }, ASK_AGAIN_MS);
    return () => clearTimeout(timer);
  });

  const send = (work: () => Promise<UploadResult>, done: string, fallback: string) => {
    setSending(true);
    return work().then((result) => settle(result, done), (error: Error) => {
      toast.error(toErrorMessage(error, fallback));
      return false;
    }).finally(() => setSending(false));
  };

  const add = (choice: BackupChoice) => send(
    () => addVolumeOffsiteServerFn({ data: { organizationSlug, environment, volumeId, choice } }),
    "Uploaded",
    "Could not add the offsite copy",
  );
  const upload = (done = "Uploaded") => send(
    () => uploadVolumeOffsiteServerFn({ data: { organizationSlug, environment, volumeId, from: null } }),
    done,
    "Upload failed",
  );

  return { read, add, upload, field, clearField: () => setField(null), pending: sending || watching !== null, uploading: watching !== null };
}
