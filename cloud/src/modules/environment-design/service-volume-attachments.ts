import { Schema, SchemaGetter } from "effect";
import type { EnvironmentResourceType } from "#/modules/environment-design/resources";
import {
  OrganizationSlug,
  Uuid,
} from "#/modules/environment-design/workspace-schemas";
import type { ServiceDeployMount } from "#/modules/environment-design/services";

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

export function getServiceMountsByServiceId(input: {
  attachments: {
    serviceId: string;
    volumeResourceId: string;
    mountPath: string;
  }[];
  volumeNameById: Map<string, string>;
}): Map<string, ServiceDeployMount[]> {
  const byServiceId = new Map<string, ServiceDeployMount[]>();
  for (const attachment of input.attachments) {
    const volumeName = input.volumeNameById.get(attachment.volumeResourceId);
    if (!volumeName) continue;
    const list = byServiceId.get(attachment.serviceId) ?? [];
    list.push({
      volumeResourceId: attachment.volumeResourceId,
      volumeName,
      mountPath: attachment.mountPath,
    });
    byServiceId.set(attachment.serviceId, list);
  }
  for (const [serviceId, mounts] of byServiceId) {
    byServiceId.set(
      serviceId,
      [...mounts].sort((left, right) =>
        left.mountPath.localeCompare(right.mountPath),
      ),
    );
  }
  return byServiceId;
}

export type AttachmentTargetError =
  | "cross_environment"
  | "non_volume_resource";

export function getAttachmentTargetError(input: {
  serviceEnvironmentId: string;
  resourceEnvironmentId: string;
  resourceImplementationType: EnvironmentResourceType;
}): AttachmentTargetError | null {
  if (input.serviceEnvironmentId !== input.resourceEnvironmentId) {
    return "cross_environment";
  }
  if (input.resourceImplementationType !== "volume") {
    return "non_volume_resource";
  }
  return null;
}

export const attachServiceVolumeSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  serviceId: Uuid,
  volumeResourceId: Uuid,
  mountPath: mountPathSchema,
});

export const detachServiceVolumeSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  serviceId: Uuid,
  volumeResourceId: Uuid,
});

export const updateServiceVolumeMountPathSchema = attachServiceVolumeSchema;

export type AttachServiceVolumeInput = typeof attachServiceVolumeSchema.Type;
export type DetachServiceVolumeInput = typeof detachServiceVolumeSchema.Type;
export type UpdateServiceVolumeMountPathInput =
  typeof updateServiceVolumeMountPathSchema.Type;
