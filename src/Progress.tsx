import type { Phase, Session } from './types';

export const busyPhases: Phase[] = ['starting', 'transcribing', 'cleaning', 'inserting'];

const stages = [
  { phase: 'transcribing', status: 'Transcribing' },
  { phase: 'cleaning', status: 'Cleaning up' },
] as const;

// Text appears as soon as insertion starts; the rest of that phase only keeps
// the clipboard available to the destination, so it already counts as done.
export function finished(session: Session) {
  return session.phase === 'inserting' || session.phase === 'done';
}

export function processing(session: Session) {
  return stages.some((stage) => stage.phase === session.phase) || finished(session);
}

export function statusLabel(session: Session) {
  if (session.phase === 'starting') return 'Starting';
  if (finished(session)) return 'Done';
  const stage = stages.find((stage) => stage.phase === session.phase);
  return stage && session.retrying ? 'Retrying' : (stage?.status ?? null);
}
