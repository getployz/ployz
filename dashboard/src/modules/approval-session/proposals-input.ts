import { Schema } from "effect";

const hexOf = (digits: number) => Schema.String.check(Schema.isPattern(new RegExp(`^[0-9a-f]{${digits}}$`)));
const Base64Url = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{1,8192}$/));
const Signature = Schema.Struct({ key: hexOf(64), signature: hexOf(128) });
const Grant = Schema.Struct({
  body: Schema.String.check(Schema.isMaxLength(4096)),
  assertion: Schema.Struct({ credential_id: Base64Url, authenticator_data: Base64Url, client_data_json: Base64Url, signature: Base64Url }),
});

/** The one envelope a browser signs for a whole proposal: every step's digest, in order. Servers check every byte. */
export const ProposalBundleInput = Schema.Struct({
  body: Schema.String.check(Schema.isMaxLength(16384)),
  signatures: Schema.Array(Signature).check(Schema.isMaxLength(4)),
  grants: Schema.optionalKey(Schema.Array(Grant).check(Schema.isMaxLength(4))),
});

/** A step's approval for one container: unsigned itself, it rides inside the bundle that names its digest. */
const StepInput = Schema.Struct({
  body: Schema.String.check(Schema.isMaxLength(4096)),
  signatures: Schema.Array(Signature).check(Schema.isMaxLength(0)),
  proposal: ProposalBundleInput,
});

/** Per step index, one approval per container ID. */
export const ProposalApprovalsInput = Schema.Record(Schema.String.check(Schema.isPattern(/^\d{1,2}$/)), Schema.Record(hexOf(64), StepInput));
