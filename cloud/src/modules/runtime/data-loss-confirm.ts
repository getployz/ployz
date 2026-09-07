import { Data, Schema } from "effect";
import {
  dataLossIdentityKey,
  dockerVolumeIdSchema,
  type DataLossIdentity,
} from "./data-loss-identity";

/** Rust re-read found identities that were not in the confirmed list. Re-open #256. */
export class MissingDataLossIdentities extends Data.TaggedError(
  "MissingDataLossIdentities",
)<{
  identities: DataLossIdentity[];
  message: string;
}> {
  constructor(identities: readonly DataLossIdentity[]) {
    super({
      identities: [...identities],
      message: "Rust reported identities that were not in the confirm list.",
    });
  }
}

/** Cloud DB rows shown in the Data Loss modal. Never sent to rust. */
export type CloudRowLoss = {
  kind: string;
  name: string;
};

export type DataLossList = {
  rust: DataLossIdentity[];
  cloud: CloudRowLoss[];
};

export const confirmedVolumeRemoveSchema = Schema.Array(dockerVolumeIdSchema);

export type ConfirmedVolumeRemove = typeof confirmedVolumeRemoveSchema.Type;

export function cloudRowKey(row: CloudRowLoss): string {
  return `${row.kind}\0${row.name}`;
}

function uniqueBy<T>(items: readonly T[], keyOf: (item: T) => string): T[] {
  const seen = new Set<string>();
  const unique: T[] = [];
  for (const item of items) {
    const key = keyOf(item);
    if (seen.has(key)) continue;
    seen.add(key);
    unique.push(item);
  }
  return unique;
}

export function unionDataLossLists(
  lists: readonly DataLossList[],
): DataLossList {
  return {
    rust: uniqueBy(
      lists.flatMap((list) => list.rust),
      dataLossIdentityKey,
    ),
    cloud: uniqueBy(
      lists.flatMap((list) => list.cloud),
      cloudRowKey,
    ),
  };
}

export function directVolumeDataLoss(
  volumes: readonly DataLossIdentity["id"][],
): DataLossList {
  return {
    rust: volumes.map((volume) => ({
      kind: "docker_volume",
      id: { ...volume },
    })),
    cloud: [],
  };
}

export function confirmedVolumeRemove(
  list: DataLossList,
): ConfirmedVolumeRemove {
  return list.rust.map((identity) => ({ ...identity.id }));
}

export function withMissingDataLossIdentities(
  list: DataLossList,
  missing: readonly DataLossIdentity[],
): DataLossList {
  return unionDataLossLists([list, { rust: [...missing], cloud: [] }]);
}
