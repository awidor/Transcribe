import { useEffect } from 'react';
import { api } from './api';
import { windowsKey } from './ShortcutField';

const MODIFIERS = new Set([160, 161, 162, 163, 164, 165, 91, 92]);
const ALT = new Set([164, 165]);

// Windows withholds input from the global shortcut listener while this window is in
// front, so the window reports its own keys. Like the listener, the key completing
// the shortcut never reaches the page, and an Alt in the shortcut must not open the
// window menu, which would take the next keys.
export function watchWindowShortcut(current: () => number[][]) {
  const held = new Set<number>();
  const swallowed = new Set<number>();
  let reports = Promise.resolve();
  const matches = (binding: number[][]) =>
    binding.length === held.size &&
    binding.every((options) => options.some((code) => held.has(code)));
  const key = (event: KeyboardEvent) => {
    const key = windowsKey(event);
    if (!key) return;
    const binding = current();
    const down = event.type === 'keydown';
    if (down && !held.has(key.code)) {
      held.add(key.code);
      if (!MODIFIERS.has(key.code) && matches(binding)) swallowed.add(key.code);
    }
    if (!down) held.delete(key.code);
    const alt = ALT.has(key.code) && binding.some((options) => options.includes(key.code));
    if (swallowed.has(key.code) || alt) event.preventDefault();
    if (!down) swallowed.delete(key.code);
    if (event.repeat) return;
    // Keep down/up ordered across IPC.
    reports = reports.then(() => api.windowShortcutKey(key, down)).catch(() => {});
  };
  const blur = () => {
    held.clear();
    swallowed.clear();
  };
  window.addEventListener('keydown', key, true);
  window.addEventListener('keyup', key, true);
  window.addEventListener('blur', blur);
  return () => {
    window.removeEventListener('keydown', key, true);
    window.removeEventListener('keyup', key, true);
    window.removeEventListener('blur', blur);
  };
}

export function useWindowShortcut(enabled: boolean, shortcut: string) {
  useEffect(() => {
    if (!enabled) return;
    let binding: number[][] = [];
    // The shortcut registers after startup, so focusing the window reloads it too.
    const load = () =>
      void api.shortcutBinding().then(
        (next) => (binding = next),
        () => {},
      );
    load();
    window.addEventListener('focus', load);
    const stop = watchWindowShortcut(() => binding);
    return () => {
      window.removeEventListener('focus', load);
      stop();
    };
  }, [enabled, shortcut]);
}
