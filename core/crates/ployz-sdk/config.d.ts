import type { ServiceConfig, ServiceSettingChange, ServiceSettingInput } from './generated/payloads';
export type * from './generated/payloads';

/** Core injects this PORT only when no authored PORT exists. */
export const DEFAULT_SERVICE_PORT: number;

export function parseServiceConfig(value: unknown): ServiceConfig;
export function parseServiceSetting<Field extends ServiceSettingInput['field']>(field: Field, value: unknown): Extract<ServiceSettingInput, { field: Field }>['value'];
export function compareServiceSettings(current: ServiceConfig, baseline: ServiceConfig | null): ServiceSettingChange[];
export function restoreServiceSetting(current: ServiceConfig, baseline: ServiceConfig, path: string): ServiceConfig;

export function resolveVariables(value: import('./generated/payloads').ResolveVariablesInput): import('./generated/payloads').ResolveVariablesResult;
export function liveValues(value: import('./generated/payloads').LiveValuesInput): import('./generated/payloads').LiveValues;

export function parseEnvironmentIntent(value: unknown): import('./generated/payloads').SavedEnvironmentIntent;
export function canonicalizeEnvironmentIntent(value: import('./generated/payloads').SavedEnvironmentIntent): import('./generated/payloads').SavedEnvironmentIntent;
export function compileEnvironmentIntent(environmentId: string, value: import('./generated/payloads').SavedEnvironmentIntent): import('./generated/payloads').CompiledEnvironmentIntent;
export function renderVariableParts(parts: import('./generated/payloads').ValuePart[], slugs: Record<string, string>): string;
export function parseSavedVariable(value: unknown): import('./generated/payloads').SavedVariableIntent;
export function restoreEnvironmentNode(current: import('./generated/payloads').SavedEnvironmentIntent, baseline: import('./generated/payloads').SavedEnvironmentIntent | null, node: { nodeType: 'service' | 'volume'; nodeId: string }, path?: string): import('./generated/payloads').SavedEnvironmentIntent;
export function parseResourceConfig(nodeType: 'volume', value: unknown): import('./generated/payloads').VolumeConfig;
export function compareResourceSettings(nodeType: 'volume', current: import('./generated/payloads').VolumeConfig, baseline: import('./generated/payloads').VolumeConfig | null): ServiceSettingChange[];
export function branchChanges(value: import('./generated/payloads').BranchChangesInput): import('./generated/payloads').BranchChanges;
export function projectEnvironmentChanges(value: import('./generated/payloads').ChangeSetInput): import('./generated/payloads').ReviewChangeSet;
export function publicationBasisMatches(basis: { kind: 'no_saved_state' } | { kind: 'saved_revision'; savedStateSnapshotId: string }, latest: string | null): boolean;
export function destructivePublication(value: unknown): { serviceIds: string[]; volumeIds: string[] };
export function destructivePublicationMismatch(value: { expected: { serviceIds: string[]; volumeIds: string[] }; reviewed: { serviceIds: string[]; volumeIds: string[] } }): string | null;
export function canonicalWorkingReview(value: unknown): string;
export function parsePublicationBasis(value: unknown): import('./generated/payloads').PublicationBasis;
export function reusePublication(input: { policy: 'always_create' | 'reuse_latest_if_equivalent'; current: { intent: import('./generated/payloads').SavedEnvironmentIntent; volumeDeletionAuthorizations: unknown }; latest: { intent: import('./generated/payloads').SavedEnvironmentIntent; volumeDeletionAuthorizations: unknown } | null }): boolean;
export function lowerDeployment(value: { namespace: string; selected?: import('./generated/payloads').ServiceAttempt[]; /** Service ID by lineage, from the frozen variable producers; references through it order the deploy. */ lineages?: Record<string, string>; snapshots: readonly { serviceId?: string; config: ServiceConfig; replicas?: number; resolvedEnv?: Record<string, string>; setupCommands?: readonly string[] }[]; volumes?: readonly { volumeResourceId: string }[] }): import('./generated/payloads').DeployIntent;

export function redactEnvironmentIntent(value: import('./generated/payloads').SavedEnvironmentIntent): import('./generated/payloads').SavedEnvironmentIntent;

export function parseRuntimePreview(value: unknown): import('./generated/payloads').DeployPreview;
export function projectRuntimeOutcome(preview: unknown, value: unknown): import('./generated/payloads').RuntimeOutcomeProjection;

export function planBranch(input: { parent: import('./generated/payloads').SavedEnvironmentIntent; deployed: string[]; focus: string[]; picks: import('./generated/payloads').BranchPicks }): import('./generated/payloads').BranchPlan;
/** Returns the accepted Namespace; throws a ConfigError saying why a name would fail at deploy. */
export function checkBranchName(name: string): string;
