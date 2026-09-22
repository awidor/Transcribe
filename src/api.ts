import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  Bootstrap,
  CleanupModel,
  Entry,
  Session,
  Settings,
  ShortcutEvent,
  ShortcutKey,
  LiveBootstrap,
  LiveSession,
  UpdateState,
} from './types';

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
  cleanupModels: () => invoke<CleanupModel[]>('cleanup_models'),
  beginShortcutCapture: () => invoke<{ token: number; platform: string }>('begin_shortcut_capture'),
  endShortcutCapture: (token: number) => invoke<void>('end_shortcut_capture', { token }),
  captureShortcutKey: (token: number, key: ShortcutKey, down: boolean) =>
    invoke<ShortcutEvent[]>('capture_shortcut_key', { token, key, down }),
  subscribeShortcut: (handler: (event: ShortcutEvent) => void) =>
    listen<ShortcutEvent>('shortcut-capture', (event) => handler(event.payload)),
  import: () => invoke<void>('import_audio'),
  liveBootstrap: () =>
    isTauri() ? invoke<LiveBootstrap>('live_bootstrap') : Promise.reject('Desktop required'),
  startLive: () => invoke<void>('start_live'),
  stopLive: () => invoke<void>('stop_live'),
  cancelLive: () => invoke<void>('cancel_live'),
  saveLiveKeys: (meta: string | null, inception: string | null) =>
    invoke<void>('save_live_keys', { meta, inception }),
  copyLive: (id: string, summary: boolean) => invoke<void>('copy_live', { id, summary }),
  deleteLive: (id: string) => invoke<void>('delete_live', { id }),
  retryLiveSummary: (id: string) => invoke<void>('retry_live_summary', { id }),
  async subscribeLive(
    onSession: (s: LiveSession) => void,
    onHistory: () => void,
    onLevel: (n: number) => void,
  ) {
    if (!isTauri()) return () => {};
    const subscriptions = await Promise.all([
      listen<LiveSession>('live-session', (e) => onSession(e.payload)),
      listen('live-history', onHistory),
      listen<number>('live-level', (e) => onLevel(e.payload)),
    ]);
    return () => subscriptions.forEach((unlisten) => unlisten());
  },
  openHistory: () => invoke<void>('open_history'),
  updateState: () =>
    isTauri() ? invoke<UpdateState>('update_state') : Promise.reject('Desktop required'),
  checkUpdate: () => invoke<UpdateState>('check_update'),
  installUpdate: () => invoke<void>('install_update'),
  subscribeUpdate: (handler: (state: UpdateState) => void) =>
    isTauri() ? listen<UpdateState>('update', (e) => handler(e.payload)) : Promise.resolve(() => {}),
  async subscribe(
    onSession: (s: Session) => void,
    onHistory: () => void,
    onLevel: (n: number) => void,
    onOpenHistory: () => void,
  ) {
    if (!isTauri()) return () => {};
    const subscriptions = await Promise.all([
      listen<Session>('session', (e) => onSession(e.payload)),
      listen('history', onHistory),
      listen<number>('level', (e) => onLevel(e.payload)),
      listen('open-history', onOpenHistory),
    ]);
    return () => subscriptions.forEach((unlisten) => unlisten());
  },
};
