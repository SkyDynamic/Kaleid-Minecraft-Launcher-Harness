import { useSyncExternalStore, type ComponentType } from 'react';

/**
 * Minimal slot registry.
 *
 * App code registers the full-featured implementations at boot (priority 10).
 * A slot always renders its built-in fallback while nothing is registered, so
 * disabling a host plugin (for example `version-manager`) genuinely removes the
 * interactive UI and leaves the read-only variant in place.
 */
export type SlotComponent = ComponentType;

interface SlotEntry {
  id: string;
  component: SlotComponent;
  priority: number;
}

const registry = new Map<string, SlotEntry[]>();
const listeners = new Set<() => void>();
let seq = 0;

function notify() {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function best(name: string): SlotComponent | null {
  const entries = registry.get(name);
  if (!entries || entries.length === 0) return null;
  let winner = entries[0]!;
  for (const entry of entries) {
    if (entry.priority > winner.priority) winner = entry;
  }
  return winner.component;
}

/** Registers a component for a slot. Returns an unregister function. */
export function registerSlot(
  name: string,
  component: SlotComponent,
  priority = 0,
  id?: string,
): () => void {
  const key = id ?? `slot-${++seq}`;
  const entries = (registry.get(name) ?? []).filter((entry) => entry.id !== key);
  entries.push({ id: key, component, priority });
  registry.set(name, entries);
  notify();
  return () => {
    const current = registry.get(name) ?? [];
    registry.set(
      name,
      current.filter((entry) => entry.id !== key),
    );
    notify();
  };
}

export function useSlot(name: string): SlotComponent | null {
  return useSyncExternalStore(
    subscribe,
    () => best(name),
    () => null,
  );
}

export interface SlotProps {
  name: string;
  fallback?: SlotComponent | null;
}

export function Slot({ name, fallback: Fallback = null }: SlotProps) {
  const Registered = useSlot(name);
  if (Registered) return <Registered />;
  if (Fallback) return <Fallback />;
  return null;
}
