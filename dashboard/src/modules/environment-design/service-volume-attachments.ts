import { Schema, SchemaGetter } from "effect";
import {
  OrganizationSlug,
  Uuid,
} from "#/modules/environment-design/workspace-schemas";

const MOUNT_PATH_MAX_LENGTH = 4096;

export const mountPathSchema = Schema.Trim.pipe(
  Schema.check(
    Schema.isNonEmpty({ message: "Mount path is required" }),
    Schema.isMaxLength(MOUNT_PATH_MAX_LENGTH, {
      message: `Mount path must be ${MOUNT_PATH_MAX_LENGTH} characters or fewer`,
    }),
    Schema.isStartsWith("/", { message: "Mount path must start with /" }),
    Schema.makeFilter(
      (value: string) => !value.includes("//"),
      { message: "Mount path must not contain //" },
    ),
  ),
  Schema.decode({
    decode: SchemaGetter.transform((value) =>
      value === "/" ? "/" : value.replace(/\/+$/, ""),
    ),
    encode: SchemaGetter.transform((value) => value),
  }),
);

export const environmentServiceVolumeAttachmentSchema = Schema.Struct({
  environmentId: Uuid,
  serviceId: Uuid,
  volumeResourceId: Uuid,
  mountPath: Schema.NonEmptyString,
});

export type EnvironmentServiceVolumeAttachment =
  typeof environmentServiceVolumeAttachmentSchema.Type;

export type ServiceMount = {
  volumeResourceId: string;
  mountPath: string;
};

export type AttachmentConflict =
  | { type: "duplicate_pair" }
  | { type: "duplicate_mount_path"; mountPath: string };

export function getMountConflict(input: {
  serviceMounts: ServiceMount[];
  volumeResourceId: string;
  mountPath: string;
  mode: "attach" | "edit";
}): AttachmentConflict | null {
  const pairExists = input.serviceMounts.some(
    (mount) => mount.volumeResourceId === input.volumeResourceId,
  );
  if (input.mode === "attach" && pairExists) {
    return { type: "duplicate_pair" };
  }

  const pathTaken = input.serviceMounts.some(
    (mount) =>
      mount.volumeResourceId !== input.volumeResourceId &&
      mount.mountPath === input.mountPath,
  );
  if (pathTaken) {
    return { type: "duplicate_mount_path", mountPath: input.mountPath };
  }

  return null;
}

export const attachServiceVolumeSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  revision: Uuid,
  environmentId: Uuid,
  serviceId: Uuid,
  volumeResourceId: Uuid,
  mountPath: mountPathSchema,
});

export const detachServiceVolumeSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  revision: Uuid,
  environmentId: Uuid,
  serviceId: Uuid,
  volumeResourceId: Uuid,
});

export const updateServiceVolumeMountPathSchema = attachServiceVolumeSchema;

export type AttachServiceVolumeInput = typeof attachServiceVolumeSchema.Type;
export type DetachServiceVolumeInput = typeof detachServiceVolumeSchema.Type;
export type UpdateServiceVolumeMountPathInput =
  typeof updateServiceVolumeMountPathSchema.Type;
