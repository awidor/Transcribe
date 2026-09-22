import { Check } from 'lucide-react';
import type { Phase, Session } from './types';

export const busyPhases: Phase[] = ['starting', 'transcribing', 'cleaning', 'inserting'];

export function stages(insert: boolean) {
  return [
    { phase: 'transcribing', label: 'Transcribe', status: 'Transcribing' },
    { phase: 'cleaning', label: 'Clean up', status: 'Cleaning up' },
    { phase: 'inserting', label: insert ? 'Write' : 'Save', status: insert ? 'Writing' : 'Saving' },
  ] as const;
}

export function processing(session: Session) {
  return stages(session.insert).some((stage) => stage.phase === session.phase) || session.phase === 'done';
}

export function statusLabel(session: Session) {
  if (session.phase === 'starting') return 'Starting';
  if (session.phase === 'done') return 'Done';
  return stages(session.insert).find((stage) => stage.phase === session.phase)?.status ?? null;
}

export function Steps({ session, labelled = false }: { session: Session; labelled?: boolean }) {
  const all = stages(session.insert);
  const current = session.phase === 'done' ? all.length : all.findIndex((s) => s.phase === session.phase);
  return (
    <ol className={labelled ? 'steps labelled' : 'steps'} aria-label="Progress">
      {all.map((stage, i) => {
        const state = i < current ? 'complete' : i === current ? 'active' : 'pending';
        return (
          <li key={stage.phase} className={state} aria-current={state === 'active' ? 'step' : undefined}>
            {labelled ? (
              <>
                <span className="step-mark" aria-hidden="true">
                  {state === 'complete' && <Check size={10} strokeWidth={3} />}
                </span>
                <span>{stage.label}</span>
              </>
            ) : (
              <span className="visually-hidden">{stage.label}</span>
            )}
          </li>
        );
      })}
    </ol>
  );
}
