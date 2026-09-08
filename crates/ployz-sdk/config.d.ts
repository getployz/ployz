import type { ServiceConfig, ServiceSettingChange, ServiceSettingInput } from './generated/payloads';
export type * from './generated/payloads';

export function parseServiceConfig(value: unknown): ServiceConfig;
export function parseServiceSetting<Field extends ServiceSettingInput['field']>(field: Field, value: unknown): Extract<ServiceSettingInput, { field: Field }>['value'];
export function compareServiceSettings(current: ServiceConfig, baseline: ServiceConfig | null): ServiceSettingChange[];
export function restoreServiceSetting(current: ServiceConfig, baseline: ServiceConfig, path: string): ServiceConfig;

export function resolveVariables(value: import('./generated/payloads').ResolveVariablesInput): import('./generated/payloads').ResolveVariablesResult;

export function parseEnvironmentIntent(value: unknown): import('./generated/payloads').SavedEnvironmentIntent;
export function canonicalizeEnvironmentIntent(value: import('./generated/payloads').SavedEnvironmentIntent): import('./generated/payloads').SavedEnvironmentIntent;
export function compileEnvironmentIntent(environmentId: string, value: import('./generated/payloads').SavedEnvironmentIntent): import('./generated/payloads').CompiledEnvironmentIntent;
export function renderVariableParts(parts: import('./generated/payloads').ValuePart[], slugs: Record<string, string>): string;
export function parseSavedVariable(value: unknown): import('./generated/payloads').SavedVariableIntent;
export function restoreEnvironmentNode(current: import('./generated/payloads').SavedEnvironmentIntent, baseline: import('./generated/payloads').SavedEnvironmentIntent | null, node: { nodeType: 'service' | 'variable_group' | 'volume'; nodeId: string }, path?: string): import('./generated/payloads').SavedEnvironmentIntent;
export function parseResourceConfig(nodeType: 'volume', value: unknown): import('./generated/payloads').VolumeConfig;
export function parseResourceConfig(nodeType: 'variable_group', value: unknown): import('./generated/payloads').VariableGroupConfig;
export function compareResourceSettings(nodeType: 'volume' | 'variable_group', current: import('./generated/payloads').VolumeConfig | import('./generated/payloads').VariableGroupConfig, baseline: import('./generated/payloads').VolumeConfig | import('./generated/payloads').VariableGroupConfig | null): ServiceSettingChange[];
export function projectEnvironmentChanges(value: import('./generated/payloads').ChangeSetInput): import('./generated/payloads').ReviewChangeSet;
export function resolveWorkingComparison<T>(input: { saved: T | null; applied: T | null; introduction: T | null }): { role: 'saved' | 'node_introduction'; value: T } | null;
export function publicationBasisMatches(basis: { kind: 'no_saved_state' } | { kind: 'saved_revision'; savedStateSnapshotId: string }, latest: string | null): boolean;
export function destructivePublication(value: unknown): { serviceIds: string[]; volumeIds: string[] };
export function destructivePublicationMismatch(value: { expected: { serviceIds: string[]; volumeIds: string[] }; reviewed: { serviceIds: string[]; volumeIds: string[] } }): string | null;
export function canonicalWorkingReview(value: unknown): string;
export function parsePublicationBasis(value: unknown): import('./generated/payloads').PublicationBasis;
export function parseSavedDiscard(value: unknown): import('./generated/payloads').SavedDiscardCommand;
export function reusePublication(input: { policy: 'always_create' | 'reuse_latest_if_equivalent'; current: { intent: import('./generated/payloads').SavedEnvironmentIntent; volumeDeletionAuthorizations: unknown }; latest: { intent: import('./generated/payloads').SavedEnvironmentIntent; volumeDeletionAuthorizations: unknown } | null }): boolean;
export function lowerDeployment(value: { projectName: string; snapshots: readonly { config: ServiceConfig; replicas?: number; resolvedEnv?: Record<string, string>; healthcheckPort?: number }[]; volumes?: readonly { volumeResourceId: string }[] }): import('./generated/payloads').DeployIntent;

export function redactEnvironmentIntent(value: import('./generated/payloads').SavedEnvironmentIntent): import('./generated/payloads').SavedEnvironmentIntent;

export function parseRuntimePreview(value: unknown): import('./generated/payloads').DeployPreview;
export function projectRuntimeOutcome(preview: unknown, value: unknown): import('./generated/payloads').RuntimeOutcomeProjection;
