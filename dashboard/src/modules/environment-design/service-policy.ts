import { Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";

/** A Service's Preferred Builder: GitHub Actions or one Server. Absent is Auto. */
export const preferredBuilderSchema = Schema.Union([Schema.Literal("github"), rustMachineIdSchema]);

/** Immediate trigger preferences. Never part of an Environment configuration. */
export const servicePolicySchema = Schema.Struct({
  autoDeploy: Schema.Boolean,
  waitForCi: Schema.Boolean,
  watchPaths: Schema.mutable(Schema.Array(Schema.Trim.check(Schema.isNonEmpty()))),
  imageUpdate: Schema.Union([
    Schema.Struct({ type: Schema.Literal("off") }),
    Schema.Struct({ type: Schema.Literal("track-tag"), tag: Schema.Trim.check(Schema.isNonEmpty(), Schema.isMaxLength(255)) }),
  ]),
  preferredBuilder: Schema.optionalKey(preferredBuilderSchema),
});
export const servicePolicyEditSchema = Schema.Struct({
  autoDeploy: Schema.optionalKey(servicePolicySchema.fields.autoDeploy),
  waitForCi: Schema.optionalKey(servicePolicySchema.fields.waitForCi),
  watchPaths: Schema.optionalKey(servicePolicySchema.fields.watchPaths),
  imageUpdate: Schema.optionalKey(servicePolicySchema.fields.imageUpdate),
  /** null goes back to Auto. */
  preferredBuilder: Schema.optionalKey(Schema.NullOr(preferredBuilderSchema)),
});
export type ServicePolicy = typeof servicePolicySchema.Type;
export const defaultServicePolicy: ServicePolicy = {
  autoDeploy: true, waitForCi: false, watchPaths: [], imageUpdate: { type: "off" },
};
