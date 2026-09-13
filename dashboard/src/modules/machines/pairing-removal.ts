import { Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";

const EncryptedCredential = Schema.Struct({
  version: Schema.Literal(1),
  iv: Schema.String,
  tag: Schema.String,
  ciphertext: Schema.String,
});

export const RemovalEndpoint = Schema.Union([
  Schema.Struct({ status: Schema.Literal("unknown"), machineId: rustMachineIdSchema }),
  Schema.Struct({ status: Schema.Literal("pending"), machineId: rustMachineIdSchema,
    encryptedExpected: EncryptedCredential }),
  Schema.Struct({ status: Schema.Literal("prepared"), machineId: rustMachineIdSchema,
    encryptedExpected: EncryptedCredential, encryptedSuccessor: EncryptedCredential }),
  Schema.Struct({ status: Schema.Literal("confirmed"), machineId: rustMachineIdSchema }),
]);

export type RemovalEndpoint = typeof RemovalEndpoint.Type;
export const RemovalEndpoints = Schema.Array(RemovalEndpoint);
