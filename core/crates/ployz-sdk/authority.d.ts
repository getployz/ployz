/**
 * The statements a person's key signs and a Server verifies, as `ployz_core::authority` writes them, and the deploy
 * approval a browser key signs. Browser code may import this entry: it runs ployz-core as WASM, with no native binding.
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
  VolumeApproval,
  VolumeWorkflow,
} from "./generated/payloads";

import type { ApprovedChange, DeployTerms, PlanView } from "./generated/payloads";

/**
 * The deploy approval a browser key signs for `plan`, a Store plan read with `target`, under `terms`: the canonical body
 * to sign as it is, what each Service shows the approver, and one line per Service. Each spec's digest is taken here, by
 * the ployz-core code the CLI and the Servers run. Throws an Error for a plan read without `target`, a Service built from
 * source, an image named by a tag, or malformed terms.
 */
export function deployApprovalBody(plan: Pick<PlanView, "namespace" | "target">, terms: DeployTerms): { body: string; changes: ApprovedChange[]; lines: string[] };
