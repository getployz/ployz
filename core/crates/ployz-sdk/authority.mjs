import { deploy_approval_body } from './generated/config-wasm.mjs';

export function deployApprovalBody(plan, terms) {
  if (plan.target === undefined) throw new Error('Read the plan with target set to sign a deploy.');
  return JSON.parse(deploy_approval_body(JSON.stringify({ namespace: plan.namespace, target: plan.target, terms })));
}
