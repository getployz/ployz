import { auto_deploy_rule_body, auto_deploy_rule_end_body, deploy_approval_body, preview_review_body, private_access_approval_body, sealing_rotation_body, starting_set_body, teardown_body, volume_approval_body } from './generated/config-wasm.mjs';

export function deployApprovalBody(plan, terms) {
  if (plan.target === undefined) throw new Error('Read the plan with target set to sign a deploy.');
  return JSON.parse(deploy_approval_body(JSON.stringify({ namespace: plan.namespace, target: plan.target, terms })));
}

export function autoDeployRuleBody(view, terms, services) {
  return JSON.parse(auto_deploy_rule_body(JSON.stringify(services === undefined ? { view, terms } : { view, terms, services })));
}

export function autoDeployRuleEndBody(view, terms) {
  return JSON.parse(auto_deploy_rule_end_body(JSON.stringify({ view, issued_at: terms.issued_at, signer: terms.signer })));
}

export function privateAccessApprovalBody(terms) {
  return private_access_approval_body(JSON.stringify(terms));
}

export function teardownBody(terms, names = {}) {
  return JSON.parse(teardown_body(JSON.stringify({ terms, servers: names.servers ?? {}, volumes: names.volumes ?? {} })));
}

export function previewReviewBody(view, terms) {
  const all = { ...terms, key: view.key, commit: view.commit, environment: view.environment.id };
  return JSON.parse(preview_review_body(JSON.stringify({ rule: view.rule, namespace: view.namespace, target: view.target, terms: all })));
}

export function volumeApprovalBody(terms) {
  return JSON.parse(volume_approval_body(JSON.stringify(terms)));
}

export function startingSetBody(terms, names = {}) {
  return JSON.parse(starting_set_body(JSON.stringify({ terms, servers: names.servers ?? {} })));
}

export function sealingRotationBody(terms, names = {}) {
  return JSON.parse(sealing_rotation_body(JSON.stringify({ terms, servers: names.servers ?? {} })));
}
