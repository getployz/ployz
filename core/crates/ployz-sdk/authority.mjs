import { auto_deploy_rule_body, auto_deploy_rule_end_body, private_access_approval_body } from './generated/config-wasm.mjs';

export function autoDeployRuleBody(view, terms, services) {
  return JSON.parse(auto_deploy_rule_body(JSON.stringify(services === undefined ? { view, terms } : { view, terms, services })));
}

export function autoDeployRuleEndBody(view, terms) {
  return JSON.parse(auto_deploy_rule_end_body(JSON.stringify({ view, issued_at: terms.issued_at, signer: terms.signer })));
}

export function privateAccessApprovalBody(terms) {
  return private_access_approval_body(JSON.stringify(terms));
}
