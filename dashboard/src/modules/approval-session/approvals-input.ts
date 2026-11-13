import { Schema } from "effect";

const hexOf = (digits: number) => Schema.String.check(Schema.isPattern(new RegExp(`^[0-9a-f]{${digits}}$`)));
const Base64Url = Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{1,8192}$/));
const SignedBody = Schema.String.check(Schema.isMaxLength(4096));

const Assertion = Schema.Struct({ credential_id: Base64Url, authenticator_data: Base64Url, client_data_json: Base64Url, signature: Base64Url });

/** A statement a passkey answered with a prompt of its own, as `ployz_core::authority::KeyApproval` travels. Servers check every byte; Cloud only bounds its size. */
export const PasskeyAnswerInput = Schema.Struct({ passkey: Schema.Struct({ body: SignedBody, assertion: Assertion }) });

/** A browser's approval as `ployz_core::authority::Signed` travels. Servers check every byte; Cloud only bounds its size. */
const ApprovalInput = Schema.Struct({
  body: SignedBody,
  signatures: Schema.Array(Schema.Struct({ key: hexOf(64), signature: hexOf(128) })).check(Schema.isMaxLength(4)),
  grants: Schema.optionalKey(Schema.Array(Schema.Struct({
    body: SignedBody,
    assertion: Assertion,
  })).check(Schema.isMaxLength(4))),
});

/** One approval per container ID, from a browser's passkey session. */
export const ApprovalsInput = Schema.Record(hexOf(64), ApprovalInput);

const TeardownBody = Schema.String.check(Schema.isMaxLength(16384));

/**
 * A person's teardown as `ployz_core::authority::TeardownApproval` travels from a person: signed by a Device's key, or
 * answered by a passkey's own prompt. Never a preview rule's, which only the Store hands its runner. Servers check every
 * byte; Cloud only bounds its size.
 */
export const TeardownInput = Schema.Union([
  Schema.Struct({
    signed: Schema.Struct({
      body: TeardownBody,
      signatures: Schema.mutable(Schema.Array(Schema.Struct({ key: hexOf(64), signature: hexOf(128) })).check(Schema.isMaxLength(4))),
    }),
  }),
  Schema.Struct({ passkey: Schema.Struct({ body: TeardownBody, assertion: Assertion }) }),
]);
