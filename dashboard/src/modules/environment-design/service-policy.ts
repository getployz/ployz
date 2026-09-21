import { Schema } from "effect";

/** Immediate trigger preferences. Never part of an Environment configuration. */
export const servicePolicySchema = Schema.Struct({
  autoDeploy: Schema.Boolean,
  waitForCi: Schema.Boolean,
  watchPaths: Schema.mutable(Schema.Array(Schema.Trim.check(Schema.isNonEmpty()))),
  imageUpdate: Schema.Union([
    Schema.Struct({ type: Schema.Literal("off") }),
    Schema.Struct({ type: Schema.Literal("track-tag"), tag: Schema.Trim.check(Schema.isNonEmpty(), Schema.isMaxLength(255)) }),
  ]),
});
export const servicePolicyEditSchema = Schema.Struct({
  autoDeploy: Schema.optionalKey(servicePolicySchema.fields.autoDeploy),
  waitForCi: Schema.optionalKey(servicePolicySchema.fields.waitForCi),
  watchPaths: Schema.optionalKey(servicePolicySchema.fields.watchPaths),
  imageUpdate: Schema.optionalKey(servicePolicySchema.fields.imageUpdate),
});
export type ServicePolicy = typeof servicePolicySchema.Type;
export const defaultServicePolicy: ServicePolicy = {
  autoDeploy: true, waitForCi: false, watchPaths: [], imageUpdate: { type: "off" },
};
