export type Phase =
  'idle' | 'starting' | 'recording' | 'transcribing' | 'cleaning' | 'inserting' | 'done' | 'error';
// Confirmed speech, then the guess that follows it.
export interface Words {
  text: string;
  interim: string;
}
export interface Session {
  phase: Phase;
  startedAt: number | null;
  error: string | null;
  retrying: boolean;
  transcript?: string | null;
  words?: Words;
}
export interface Entry {
  id: string;
  createdAt: number;
  text: string;
  seconds: number;
  status: string;
  error: string | null;
}
export type ReasoningEffort = 'none' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh' | 'max';
export interface CleanupModel {
  id: string;
  name: string;
  reasoningEfforts: ReasoningEffort[];
}
export type CleanupEngine = 'off' | 'openrouter' | 's1-mini';
export type S1Part = 'engine' | 'model';
export interface S1Download {
  ready: boolean;
  size: number;
  progress: number | null;
  error: string | null;
}
export interface S1Status {
  engine: S1Download;
  model: S1Download;
}
export type Styling = 'casual' | 'semi-casual' | 'semi-formal' | 'formal';
export interface Settings {
  microphone: string | null;
  shortcut: string;
  shortcutLabel?: string | null;
  cleanupModel: string;
  cleanupReasoningEffort: ReasoningEffort | null;
  cleanupEngine: CleanupEngine;
  cleanupStyling: Styling;
  cleanupUnloadSeconds: number | null;
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
  hasAzureKey: boolean;
  hasOpenRouterKey: boolean;
  session: Session;
}
export interface UpdateState {
  phase: 'unsupported' | 'idle' | 'checking' | 'current' | 'available' | 'installing' | 'error';
  current: string;
  version: string | null;
  error: string | null;
}
