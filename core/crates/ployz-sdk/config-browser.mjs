import { config_request } from './generated/config-wasm.mjs';

const request = input => JSON.parse(config_request(JSON.stringify(input)));
export const parseServiceConfig = value => request({ operation: 'parse_service', value });
export const parseServiceSetting = (field, value) => request({ operation: 'parse_setting', value: { field, value } });
export const compareServiceSettings = (current, baseline) => request({ operation: 'compare_service', current, baseline });
export const restoreServiceSetting = (current, baseline, path) => request({ operation: 'restore_service', current, baseline, path });

export const resolveVariables = value => request({ operation: 'resolve_variables', value });

export const parseEnvironmentIntent = value => request({ operation: 'parse_environment', value });
export const canonicalizeEnvironmentIntent = value => request({ operation: 'canonicalize_environment', value });
export const compileEnvironmentIntent = (environmentId, value) => request({ operation: 'compile_environment', environment_id: environmentId, value });
export const renderVariableParts = (parts, slugs) => request({ operation: 'render_variable_parts', parts, slugs });
export const parseSavedVariable = value => request({ operation: 'parse_saved_variable', value });
export const restoreEnvironmentNode = (current, baseline, node, path = null) => request({ operation: 'restore_environment', current, baseline, node_type: node.nodeType, node_id: node.nodeId, path });
export const parseResourceConfig = (nodeType, value) => request({ operation: 'parse_resource', node_type: nodeType, value });
export const compareResourceSettings = (nodeType, current, baseline) => request({ operation: 'compare_resource', node_type: nodeType, current, baseline });
export const projectEnvironmentChanges = value => request({ operation: 'project_changes', value });
export const publicationBasisMatches = (basis, latest) => request({ operation: 'publication_basis_matches', basis, latest });
export const destructivePublication = value => request({ operation: 'destructive_publication', value });
export const destructivePublicationMismatch = input => request({ operation: 'destructive_publication_mismatch', ...input });
export const canonicalWorkingReview = value => request({ operation: 'canonical_working_review', value });
export const parsePublicationBasis = value => request({ operation: 'parse_publication_basis', value });
export const reusePublication = input => request({ operation: 'reuse_publication', ...input });
export const lowerDeployment = value => request({ operation: 'lower_deployment', value });

export const redactEnvironmentIntent = value => request({ operation: 'redact_environment', value });

export const parseRuntimePreview = value => request({ operation: 'parse_runtime_preview', value });
export const projectRuntimeOutcome = (preview, value) => request({ operation: 'project_runtime_outcome', preview, value });
