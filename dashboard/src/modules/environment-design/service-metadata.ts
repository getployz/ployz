import { Schema } from "effect";
import { Uuid, OrganizationSlug } from "./workspace-schemas";
import { servicePolicyEditSchema } from "./service-policy";

export const serviceMetadataEditSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  serviceId: Uuid,
  edit: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("rename"), name: Schema.Trim.check(Schema.isNonEmpty(), Schema.isMaxLength(64)) }),
    Schema.Struct({ kind: Schema.Literal("policy"), policy: servicePolicyEditSchema }),
  ]),
});
export type ServiceMetadataEdit = typeof serviceMetadataEditSchema.Type;
