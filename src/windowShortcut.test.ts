import { cleanup, fireEvent, renderHook, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { api } from './api';
import { useWindowShortcut, watchWindowShortcut } from './windowShortcut';

vi.mock('./api', () => ({
  api: {
    shortcutBinding: vi.fn(async () => [[165]]),
    windowShortcutKey: vi.fn(async () => {}),
  },
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
const rightAlt = { key: 'Alt', code: 'AltRight', location: 2 };
const ctrl = { key: 'Control', code: 'ControlLeft', location: 1 };
const shift = { key: 'Shift', code: 'ShiftLeft', location: 1 };
const space = { key: ' ', code: 'Space', keyCode: 32 };
const reported = () =>
  vi.mocked(api.windowShortcutKey).mock.calls.map(([key, down]) => [key.code, down]);

describe('shortcut input in the focused window', () => {
  it('reports keys in order and keeps a bare Alt shortcut out of the window menu', async () => {
    const stop = watchWindowShortcut(() => [[165]]);
    expect(fireEvent.keyDown(window, rightAlt)).toBe(false);
    expect(fireEvent.keyDown(window, { ...rightAlt, repeat: true })).toBe(false);
    expect(fireEvent.keyUp(window, rightAlt)).toBe(false);
    await waitFor(() =>
      expect(reported()).toEqual([
        [165, true],
        [165, false],
      ]),
    );
    stop();
    expect(fireEvent.keyDown(window, rightAlt)).toBe(true);
  });

  it('withholds only the key that completes the shortcut', async () => {
    const stop = watchWindowShortcut(() => [[162, 163], [160, 161], [32]]);
    expect(fireEvent.keyDown(window, space)).toBe(true);
    expect(fireEvent.keyUp(window, space)).toBe(true);
    expect(fireEvent.keyDown(window, ctrl)).toBe(true);
    expect(fireEvent.keyDown(window, shift)).toBe(true);
    expect(fireEvent.keyDown(window, space)).toBe(false);
    expect(fireEvent.keyUp(window, space)).toBe(false);
    expect(fireEvent.keyUp(window, shift)).toBe(true);
    expect(fireEvent.keyUp(window, ctrl)).toBe(true);
    await waitFor(() => expect(reported()).toHaveLength(8));
    // Keys still down when the window loses focus are not part of the next chord.
    fireEvent.keyDown(window, ctrl);
    fireEvent.keyDown(window, shift);
    fireEvent.blur(window);
    expect(fireEvent.keyDown(window, space)).toBe(true);
    stop();
  });

  it('loads the current shortcut and reloads it when the window is focused', async () => {
    const press = (key: object) => {
      const allowed = fireEvent.keyDown(window, key);
      fireEvent.keyUp(window, key);
      return allowed;
    };
    const { unmount } = renderHook(() => useWindowShortcut(true, 'shortcut'));
    await waitFor(() => expect(press(rightAlt)).toBe(false));
    vi.mocked(api.shortcutBinding).mockResolvedValueOnce([[32]]);
    fireEvent.focus(window);
    await waitFor(() => expect(press(space)).toBe(false));
    unmount();
    expect(press(space)).toBe(true);
  });

  it('stays out of the way on other platforms', () => {
    renderHook(() => useWindowShortcut(false, 'shortcut'));
    expect(api.shortcutBinding).not.toHaveBeenCalled();
    expect(fireEvent.keyDown(window, rightAlt)).toBe(true);
  });
});
