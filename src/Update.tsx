import { useEffect, useState } from 'react';
import { LoaderCircle } from 'lucide-react';
import { api } from './api';
import type { UpdateState } from './types';

export function useUpdate(enabled: boolean) {
  const [state, setState] = useState<UpdateState | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let unsubscribe = () => {};
    void (async () => {
      unsubscribe = await api.subscribeUpdate((next) => {
        if (!disposed) setState(next);
      });
      if (disposed) {
        unsubscribe();
        return;
      }
      const next = await api.updateState();
      if (!disposed) setState((old) => old ?? next);
    })().catch(() => {});
    return () => {
      disposed = true;
      unsubscribe();
    };
  }, [enabled]);
  return state;
}

const statuses: Partial<Record<UpdateState['phase'], string>> = {
  checking: 'Checking',
  current: 'Up to date',
  installing: 'Installing',
  error: 'Update failed',
};

export function UpdatePanel({
  state,
  recording,
  act,
}: {
  state: UpdateState | null;
  recording: boolean;
  act: (fn: () => Promise<void>) => void;
}) {
  if (!state || state.phase === 'unsupported') return null;
  const busy = state.phase === 'checking' || state.phase === 'installing';
  const available = state.phase === 'available';
  const status = available ? `Version ${state.version} available` : statuses[state.phase];
  return (
    <section className="preferences">
      <div className="settings-card">
        <div className="setting-row">
          <span className="setting-heading">Version {state.current}</span>
          <span
            className="setting-control update-status"
            role="status"
            title={state.phase === 'error' ? (state.error ?? undefined) : undefined}
          >
            {status}
          </span>
          <button
            className="secondary-button"
            disabled={busy || (available && recording)}
            onClick={() =>
              act(async () => {
                if (available) await api.installUpdate();
                else await api.checkUpdate();
              })
            }
          >
            {busy && <LoaderCircle size={12} className="spin" />}
            {available || state.phase === 'installing' ? 'Install and restart' : 'Check for updates'}
          </button>
        </div>
      </div>
    </section>
  );
}
