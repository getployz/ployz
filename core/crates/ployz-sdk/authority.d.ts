/**
 * The statements a person's key signs and a Server verifies, as `ployz_core::authority` writes them. Types only, so
 * browser code may import them. `deployApprovalBody` from `@ployz/sdk` builds a deploy approval's body on the server.
 */
export type {
  ApprovedChange,
  ApprovedService,
  DeployApproval,
  DeployTerms,
  GrantBody,
  PayloadDigest,
  ProposalApproval,
  ProposalRef,
  ProposalStep,
  RestartApproval,
  SessionGrant,
  Signed,
  SignerKey,
  StepDigest,
  StepKind,
} from "./generated/payloads";
