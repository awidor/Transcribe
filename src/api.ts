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
  S1Part,
  S1Status,
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
  save: (settings: Settings, key: string | null, azureKey: string | null) =>
    invoke<Settings>('save_settings', { settings, key, azureKey }),
  cleanupModels: () => invoke<CleanupModel[]>('cleanup_models'),
  s1Status: () => invoke<S1Status>('s1_status'),
  downloadS1: (part: S1Part) => invoke<void>('download_s1', { part }),
  cancelS1Download: (part: S1Part) => invoke<void>('cancel_s1_download', { part }),
  subscribeS1: (handler: (status: S1Status) => void) =>
    isTauri() ? listen<S1Status>('s1', (e) => handler(e.payload)) : Promise.resolve(() => {}),
  beginShortcutCapture: () => invoke<{ token: number; platform: string }>('begin_shortcut_capture'),
  endShortcutCapture: (token: number) => invoke<void>('end_shortcut_capture', { token }),
  grantKeyboardAccess: () => invoke<boolean>('grant_keyboard_access'),
  captureShortcutKey: (token: number, key: ShortcutKey, down: boolean) =>
    invoke<ShortcutEvent[]>('capture_shortcut_key', { token, key, down }),
  subscribeShortcut: (handler: (event: ShortcutEvent) => void) =>
    listen<ShortcutEvent>('shortcut-capture', (event) => handler(event.payload)),
  shortcutBinding: () => invoke<number[][]>('shortcut_binding'),
  windowShortcutKey: (key: ShortcutKey, down: boolean) =>
    invoke<void>('window_shortcut_key', { key, down }),
  import: () => invoke<void>('import_audio'),
  openHistory: () => invoke<void>('open_history'),
  updateState: () =>
    isTauri() ? invoke<UpdateState>('update_state') : Promise.reject('Desktop required'),
  checkUpdate: () => invoke<UpdateState>('check_update'),
  installUpdate: () => invoke<void>('install_update'),
  subscribeUpdate: (handler: (state: UpdateState) => void) =>
    isTauri()
      ? listen<UpdateState>('update', (e) => handler(e.payload))
      : Promise.resolve(() => {}),
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
