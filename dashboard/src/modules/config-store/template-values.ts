import { useSyncExternalStore } from "react";
import type { EnvironmentRef } from "@ployz/sdk";
import { branchQuery, environmentSettingsQuery, requireView, servicesQuery, useStoreViews } from "./store-view.queries";
import { installs, type InstallRecord, type MockServer } from "./templates";

// ponytail: Install records and the mock runtime's servers live in this browser until the Config Store keeps
// Installs and Cloud observes Machines for them.

/** What this browser keeps for one Environment: its Install records by name, and the mock runtime's servers. */
export type TemplateState = { installs: Readonly<Record<string, InstallRecord>>; servers: readonly MockServer[] };

const SERVERS: readonly MockServer[] = ["m1", "m2", "m3"].map((name) => ({ name, online: true }));
const EMPTY: TemplateState = { installs: {}, servers: SERVERS };

const listeners = new Set<() => void>();
let version = 0;
const cache = new Map<string, { raw: string | null; state: TemplateState }>();

const key = (organization: string, environment: EnvironmentRef) =>
  `ployz.templates.v3:${organization}/${environment.project}/${environment.environment}`;

function subscribe(listener: () => void) {
  listeners.add(listener);
  const storage = () => { version += 1; listener(); };
  window.addEventListener("storage", storage);
  return () => { listeners.delete(listener); window.removeEventListener("storage", storage); };
}

function read(organization: string, environment: EnvironmentRef): TemplateState {
  const storageKey = key(organization, environment);
  const raw = window.localStorage.getItem(storageKey);
  const hit = cache.get(storageKey);
  if (hit && hit.raw === raw) return hit.state;
  let state = EMPTY;
  try {
    // SAFETY: only saveTemplateState writes this key, from a TemplateState.
    if (raw) state = JSON.parse(raw) as TemplateState;
  } catch { state = EMPTY; }
  cache.set(storageKey, { raw, state });
  return state;
}

export function saveTemplateState(organization: string, environment: EnvironmentRef, change: (state: TemplateState) => TemplateState) {
  window.localStorage.setItem(key(organization, environment), JSON.stringify(change(read(organization, environment))));
  version += 1;
  for (const listener of listeners) listener();
}

export const saveInstall = (organization: string, environment: EnvironmentRef, record: InstallRecord) =>
  saveTemplateState(organization, environment, (state) => ({ ...state, installs: { ...state.installs, [record.install.name]: record } }));

/** This Environment's template state; null until the browser has read it, so the server and first render agree. */
export function useTemplateState(organization: string, environment: EnvironmentRef): TemplateState | null {
  const seen = useSyncExternalStore(subscribe, () => version, () => -1);
  return seen < 0 ? null : read(organization, environment);
}

/**
 * The Installs here as screens read them, with this browser's state. On a Branch, `parent` names the Environment it
 * was made from, which edits can come from.
 */
export function useInstalls(organization: string, environment: EnvironmentRef) {
  const [services, settings, branch] = useStoreViews(organization,
    [servicesQuery(environment), environmentSettingsQuery(environment), branchQuery(environment)] as const);
  const parent = branch.ok ? branch.value.parent : null;
  const parentSettings = useStoreViews(organization, parent ? [environmentSettingsQuery({ ...environment, environment: parent })] : [])[0];
  const state = useTemplateState(organization, environment);
  const listed = requireView(services).services;
  const views = installs(listed, requireView(settings), state?.installs ?? null, parentSettings?.ok ? parentSettings.value : null);
  return { installs: views, parent, state, services: listed, settings: requireView(settings) };
}
