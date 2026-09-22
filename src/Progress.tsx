import { Check } from 'lucide-react';
import type { Phase, Session } from './types';

export const busyPhases: Phase[] = ['starting', 'transcribing', 'cleaning', 'inserting'];

const stages = [
  { phase: 'transcribing', label: 'Transcribe', status: 'Transcribing' },
  { phase: 'cleaning', label: 'Clean up', status: 'Cleaning up' },
] as const;

// Text appears as soon as insertion starts; the rest of that phase only keeps
// the clipboard available to the destination, so it is not shown as a step.
export function finished(session: Session) {
  return session.phase === 'inserting' || session.phase === 'done';
}

export function processing(session: Session) {
  return stages.some((stage) => stage.phase === session.phase) || finished(session);
}

export function statusLabel(session: Session) {
  if (session.phase === 'starting') return 'Starting';
  if (finished(session)) return 'Done';
  return stages.find((stage) => stage.phase === session.phase)?.status ?? null;
}

export function Steps({ session, labelled = false }: { session: Session; labelled?: boolean }) {
  const current = finished(session) ? stages.length : stages.findIndex((s) => s.phase === session.phase);
  return (
    <ol className={labelled ? 'steps labelled' : 'steps'} aria-label="Progress">
      {stages.map((stage, i) => {
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
