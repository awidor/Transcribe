export type Phase =
  'idle' | 'starting' | 'recording' | 'transcribing' | 'inserting' | 'done' | 'error';
export interface Session {
  phase: Phase;
  startedAt: number | null;
  error: string | null;
}
export interface Entry {
  id: string;
  createdAt: number;
  text: string;
  seconds: number;
  status: string;
  error: string | null;
}
export interface Settings {
  microphone: string | null;
  shortcut: string;
  shortcutLabel?: string | null;
}
export interface ShortcutKey {
  code: number;
  label: string;
}
export type ShortcutEvent =
  | { kind: 'capture'; token: number; keys: ShortcutKey[]; shortcut: string | null }
  | { kind: 'cancelled'; token: number };
export interface Bootstrap {
  entries: Entry[];
  settings: Settings;
  microphones: string[];
  hasKey: boolean;
  session: Session;
}
