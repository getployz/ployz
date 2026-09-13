import { Schema, Result } from "effect";
import type { VolumeRemoveOutcome, VolumeRemoveVolume } from "./volume-removal";

const Removal = Schema.Struct({
  id: Schema.Struct({ machine_id: Schema.String, name: Schema.String }),
  outcome: Schema.Union([
    Schema.Struct({ status: Schema.Literal("removed") }),
    Schema.Struct({ status: Schema.Literal("omitted") }),
    Schema.Struct({
      status: Schema.Literal("failed"),
      error: Schema.Struct({ code: Schema.String, message: Schema.String, details: Schema.Json }),
    }),
  ]),
});

export function parseVolumeRemoveOutcome<T>(
  value: T,
  requested: readonly VolumeRemoveVolume[],
): VolumeRemoveOutcome {
  const decoded = Schema.decodeUnknownResult(Schema.Array(Removal))(value, {
    onExcessProperty: "error",
  });
  if (Result.isFailure(decoded)) {
    return { destroyed: [], failed: [], omitted: [...requested] };
  }
  const result: VolumeRemoveOutcome = { destroyed: [], failed: [], omitted: [] };
  for (const volume of requested) {
    const matches = decoded.success.filter(({ id }) =>
      id.machine_id === volume.machine_id && id.name === volume.name,
    );
    const outcome = matches.length === 1 ? matches[0]?.outcome : undefined;
    switch (outcome?.status) {
      case "removed":
        result.destroyed.push(volume);
        break;
      case "failed":
        result.failed.push({ ...volume, message: outcome.error.message });
        break;
      default:
        result.omitted.push(volume);
    }
  }
  return result;
}
