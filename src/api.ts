import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { Bootstrap, Entry, Session, Settings, ShortcutEvent, ShortcutKey } from './types';

export const api = {
  bootstrap: () =>
    isTauri() ? invoke<Bootstrap>('bootstrap') : Promise.reject('Desktop required'),
  toggle: () => invoke<void>('toggle'),
  cancel: () => invoke<void>('cancel'),
  history: () => invoke<Entry[]>('history'),
  copy: (id: string) => invoke<void>('copy_entry', { id }),
  edit: (id: string, text: string) => invoke<void>('edit_entry', { id, text }),
  delete: (id: string) => invoke<void>('delete_entry', { id }),
  save: (settings: Settings, key: string | null) =>
    invoke<Settings>('save_settings', { settings, key }),
  beginShortcutCapture: () => invoke<{ token: number; platform: string }>('begin_shortcut_capture'),
  endShortcutCapture: (token: number) => invoke<void>('end_shortcut_capture', { token }),
  captureShortcutKey: (token: number, key: ShortcutKey, down: boolean) =>
    invoke<ShortcutEvent[]>('capture_shortcut_key', { token, key, down }),
  subscribeShortcut: (handler: (event: ShortcutEvent) => void) =>
    listen<ShortcutEvent>('shortcut-capture', (event) => handler(event.payload)),
  import: () => invoke<void>('import_audio'),
  retry: (id: string) => invoke<void>('retry', { id }),
  openHistory: () => invoke<void>('open_history'),
  async subscribe(
    onSession: (s: Session) => void,
    onHistory: () => void,
    onLevel: (n: number) => void,
  ) {
    if (!isTauri()) return () => {};
    const subscriptions = await Promise.all([
      listen<Session>('session', (e) => onSession(e.payload)),
      listen('history', onHistory),
      listen<number>('level', (e) => onLevel(e.payload)),
    ]);
    return () => subscriptions.forEach((unlisten) => unlisten());
  },
};
