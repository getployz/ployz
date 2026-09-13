import type { DockerVolumeName, MachineId } from "@ployz/sdk";
import { Schema } from "effect";

export type DataLossIdentity = {
  kind: "docker_volume";
  id: {
    machine_id: MachineId;
    name: DockerVolumeName;
  };
};

const MachineIdType = Schema.declare<MachineId>(
  (value): value is MachineId => typeof value === "string",
);

const DockerVolumeNameType = Schema.declare<DockerVolumeName>(
  (value): value is DockerVolumeName => typeof value === "string",
);

export const dockerVolumeIdSchema = Schema.Struct({
  machine_id: Schema.String.check(Schema.isNonEmpty()).pipe(
    Schema.decodeTo(MachineIdType),
  ),
  name: Schema.String.check(Schema.isNonEmpty()).pipe(
    Schema.decodeTo(DockerVolumeNameType),
  ),
});

export const dataLossIdentitySchema = Schema.Struct({
  kind: Schema.Literal("docker_volume"),
  id: dockerVolumeIdSchema,
});

export function dataLossIdentityKey(identity: DataLossIdentity): string {
  return `${identity.kind}\0${identity.id.machine_id}\0${identity.id.name}`;
}

export function dataLossIdentityLabel(identity: DataLossIdentity): string {
  return `${identity.id.name} on ${identity.id.machine_id}`;
}
