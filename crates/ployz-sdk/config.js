const { configRequest: request } = require('./index.js');

exports.parseServiceConfig = value => request({ operation: 'parse_service', value });
exports.parseServiceSetting = (field, value) => request({ operation: 'parse_setting', value: { field, value } });
exports.compareServiceSettings = (current, baseline) => request({ operation: 'compare_service', current, baseline });
exports.restoreServiceSetting = (current, baseline, path) => request({ operation: 'restore_service', current, baseline, path });

exports.resolveVariables = value => request({ operation: 'resolve_variables', value });

exports.parseEnvironmentIntent = value => request({ operation: 'parse_environment', value });
exports.canonicalizeEnvironmentIntent = value => request({ operation: 'canonicalize_environment', value });
exports.compileEnvironmentIntent = (environmentId, value) => request({ operation: 'compile_environment', environment_id: environmentId, value });
exports.renderVariableParts = (parts, slugs) => request({ operation: 'render_variable_parts', parts, slugs });
exports.parseSavedVariable = value => request({ operation: 'parse_saved_variable', value });
exports.restoreEnvironmentNode = (current, baseline, node, path = null) => request({ operation: 'restore_environment', current, baseline, node_type: node.nodeType, node_id: node.nodeId, path });
exports.parseResourceConfig = (nodeType, value) => request({ operation: 'parse_resource', node_type: nodeType, value });
exports.compareResourceSettings = (nodeType, current, baseline) => request({ operation: 'compare_resource', node_type: nodeType, current, baseline });
exports.projectEnvironmentChanges = value => request({ operation: 'project_changes', value });
exports.resolveWorkingComparison = input => request({ operation: 'working_comparison', ...input });
exports.publicationBasisMatches = (basis, latest) => request({ operation: 'publication_basis_matches', basis, latest });
exports.destructivePublication = value => request({ operation: 'destructive_publication', value });
exports.destructivePublicationMismatch = input => request({ operation: 'destructive_publication_mismatch', ...input });
exports.canonicalWorkingReview = value => request({ operation: 'canonical_working_review', value });
exports.parsePublicationBasis = value => request({ operation: 'parse_publication_basis', value });
exports.parseSavedDiscard = value => request({ operation: 'parse_saved_discard', value });
exports.reusePublication = input => request({ operation: 'reuse_publication', ...input });
exports.lowerDeployment = value => request({ operation: 'lower_deployment', value });

exports.redactEnvironmentIntent = value => request({ operation: 'redact_environment', value });

exports.parseRuntimePreview = value => request({ operation: 'parse_runtime_preview', value });
exports.projectRuntimeOutcome = (preview, value) => request({ operation: 'project_runtime_outcome', preview, value });
