import { use } from "react";
import { dockerVolumeName, volumeFill } from "./node-status";
import { RuntimeLensContext } from "./RuntimeLensProvider";

/** How full a Volume here is, by its id, as `volumeFill` reads it; none when the Environment has no Namespace to name it by. */
export function useVolumeFill() {
  const { volumes, namespace } = use(RuntimeLensContext);
  return (volumeId: string) => namespace === null ? null : volumeFill(volumes, dockerVolumeName(namespace, volumeId));
}
