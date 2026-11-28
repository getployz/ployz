import { auto_deploy_rule_end_body, commit_approval_body, install_key, passkey_join_step, preview_rule_body, private_access_approval_body, source_need, trash_approval_body, version_steps } from './generated/config-wasm.mjs';

export function autoDeployRuleEndBody(view, terms) {
  return JSON.parse(auto_deploy_rule_end_body(JSON.stringify({ view, issued_at: terms.issued_at, signer: terms.signer })));
}

export function previewRuleBody(view, terms) {
  return JSON.parse(preview_rule_body(JSON.stringify({ view, terms })));
}

export function previewRuleEndBody(view, terms) {
  return JSON.parse(auto_deploy_rule_end_body(JSON.stringify({ view, issued_at: terms.issued_at, signer: terms.signer })));
}

export function commitApprovalBody(approval) {
  return commit_approval_body(JSON.stringify(approval));
}

export function privateAccessApprovalBody(terms) {
  return private_access_approval_body(JSON.stringify(terms));
}

export function trashApprovalBody(terms) {
  return trash_approval_body(JSON.stringify(terms));
}

export function passkeyJoinStep(join, operation, issuedAt, expiresAt) {
  return JSON.parse(passkey_join_step(JSON.stringify({ join, operation, issued_at: issuedAt, expires_at: expiresAt })));
}

export function versionSteps(asked) {
  return JSON.parse(version_steps(JSON.stringify(asked)));
}

export function sourceNeed(view) {
  return JSON.parse(source_need(JSON.stringify(view)));
}

export function installKey(passkey) {
  return install_key(JSON.stringify(passkey));
}
