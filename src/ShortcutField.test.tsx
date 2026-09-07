import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ShortcutField, shortcutLabels, windowsKey } from './ShortcutField';
import { api } from './api';
import type { ShortcutEvent } from './types';

vi.mock('./api', () => ({
  api: {
    subscribeShortcut: vi.fn(),
    beginShortcutCapture: vi.fn(),
    endShortcutCapture: vi.fn(async () => {}),
    captureShortcutKey: vi.fn(async () => []),
  },
}));
let receive: (event: ShortcutEvent) => void;
const unlisten = vi.fn();
beforeEach(() => {
  vi.mocked(api.subscribeShortcut).mockImplementation(async (callback) => {
    receive = callback;
    return unlisten;
  });
  vi.mocked(api.beginShortcutCapture).mockResolvedValue({ token: 7, platform: 'windows' });
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
function setup() {
  const onChange = vi.fn();
  const onCapturing = vi.fn();
  const result = render(
    <ShortcutField
      value="CommandOrControl+Shift+Space"
      disabled={false}
      onChange={onChange}
      onCapturing={onCapturing}
    />,
  );
  return { ...result, onChange, onCapturing };
}
async function begin() {
  fireEvent.click(screen.getByRole('button', { name: 'Shortcut' }));
  await screen.findByText('Press keys');
}
describe('shortcut capture', () => {
  it('records focused Windows input through ordered IPC when no hook events arrive', async () => {
    const { onChange } = setup();
    const keys = [{ code: 162, label: 'Left Ctrl' }];
    const shortcut = JSON.stringify({ platform: 'windows', keys });
    vi.mocked(api.captureShortcutKey).mockImplementation(async (token, key, down) => [
      { kind: 'capture', token, keys: [key], shortcut: down ? null : shortcut },
    ]);
    await begin();
    fireEvent.keyDown(window, { key: 'Control', code: 'ControlLeft', location: 1, keyCode: 17 });
    fireEvent.keyUp(window, { key: 'Control', code: 'ControlLeft', location: 1, keyCode: 17 });
    await waitFor(() => expect(onChange).toHaveBeenCalledExactlyOnceWith(shortcut));
    expect(vi.mocked(api.captureShortcutKey).mock.calls).toEqual([
      [7, keys[0], true],
      [7, keys[0], false],
    ]);
  });
  it('maps modifier sides, numpad Enter and layout keys to native Windows codes', () => {
    expect(windowsKey(new KeyboardEvent('keydown', { key: 'Alt', location: 2 }))).toEqual({
      code: 165,
      label: 'Right Alt',
    });
    expect(
      windowsKey(new KeyboardEvent('keydown', { key: 'AltGraph', code: 'AltRight', location: 2 })),
    ).toEqual({ code: 165, label: 'Right Alt' });
    expect(windowsKey(new KeyboardEvent('keydown', { key: 'Enter', code: 'NumpadEnter' }))).toEqual(
      { code: 269, label: 'Numpad Enter' },
    );
    expect(windowsKey(new KeyboardEvent('keydown', { key: ';', keyCode: 186 }))).toEqual({
      code: 186,
      label: ';',
    });
    expect(windowsKey(new KeyboardEvent('keydown', { key: 'Process', keyCode: 229 }))).toBeNull();
  });
  it.each(['Left Ctrl', 'Right Alt', 'F24', 'Space'])(
    'records %s alone from native input',
    async (label) => {
      const { onChange, onCapturing } = setup();
      await begin();
      const keys = [{ code: 162, label }];
      act(() => receive({ kind: 'capture', token: 7, keys, shortcut: null }));
      expect(onChange).not.toHaveBeenCalled();
      expect(screen.getByText(label)).toBeInTheDocument();
      const shortcut = JSON.stringify({ platform: 'windows', keys });
      act(() => receive({ kind: 'capture', token: 7, keys, shortcut }));
      expect(onChange).toHaveBeenCalledExactlyOnceWith(shortcut);
      expect(onCapturing).toHaveBeenLastCalledWith(false);
      expect(unlisten).toHaveBeenCalled();
    },
  );
  it('keeps every key in a native chord', async () => {
    const { onChange } = setup();
    await begin();
    const keys = [
      { code: 162, label: 'Left Ctrl' },
      { code: 65, label: 'A' },
      { code: 66, label: 'B' },
    ];
    const shortcut = JSON.stringify({ platform: 'windows', keys });
    act(() => receive({ kind: 'capture', token: 7, keys, shortcut }));
    expect(JSON.parse(onChange.mock.calls[0][0]).keys).toEqual(keys);
  });
  it.each(['cancel', 'blur', 'timeout'])('%s retains the previous shortcut', async (reason) => {
    const { onChange } = setup();
    await begin();
    if (reason === 'cancel')
      fireEvent.click(screen.getByRole('button', { name: 'Cancel shortcut' }));
    else if (reason === 'blur') fireEvent.blur(window);
    else act(() => receive({ kind: 'cancelled', token: 7 }));
    act(() => receive({ kind: 'capture', token: 7, keys: [], shortcut: 'stale' }));
    expect(onChange).not.toHaveBeenCalled();
    expect(api.endShortcutCapture).toHaveBeenCalledWith(7);
    expect(screen.getByText('Space')).toBeInTheDocument();
  });
  it('cleans up a capture whose start finishes after unmount', async () => {
    let resolve!: (value: { token: number; platform: string }) => void;
    vi.mocked(api.beginShortcutCapture).mockImplementation(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const { unmount } = setup();
    fireEvent.click(screen.getByRole('button', { name: 'Shortcut' }));
    await waitFor(() => expect(api.beginShortcutCapture).toHaveBeenCalled());
    unmount();
    await act(async () => resolve({ token: 9, platform: 'windows' }));
    expect(api.endShortcutCapture).toHaveBeenCalledWith(9);
    expect(unlisten).toHaveBeenCalled();
  });
  it('shows native permission errors without changing the binding', async () => {
    vi.mocked(api.beginShortcutCapture).mockRejectedValue('Input Monitoring permission required');
    const { onChange } = setup();
    fireEvent.click(screen.getByRole('button', { name: 'Shortcut' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Input Monitoring permission required',
    );
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Shortcut' })).toBeEnabled();
  });
  it('ignores events belonging to an older capture', async () => {
    const { onChange } = setup();
    await begin();
    act(() => receive({ kind: 'capture', token: 6, keys: [], shortcut: 'stale' }));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Cancel shortcut' })).toBeInTheDocument();
  });
  it('allows Tab and Escape as keys in the Wayland recorder', async () => {
    vi.mocked(api.beginShortcutCapture).mockResolvedValue({ token: 7, platform: 'wayland' });
    const { onChange } = setup();
    await begin();
    fireEvent.keyDown(window, { key: 'Tab', code: 'Tab' });
    fireEvent.keyDown(window, { key: 'Escape', code: 'Escape' });
    fireEvent.keyUp(window, { key: 'Tab', code: 'Tab' });
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.keyUp(window, { key: 'Escape', code: 'Escape' });
    expect(
      JSON.parse(onChange.mock.calls[0][0]).keys.map((k: { label: string }) => k.label),
    ).toEqual(['Tab', 'Escape']);
  });
  it('rejects sequential input during a Wayland chord', async () => {
    vi.mocked(api.beginShortcutCapture).mockResolvedValue({ token: 7, platform: 'wayland' });
    const { onChange } = setup();
    await begin();
    for (const [type, code] of [
      ['keydown', 'ControlLeft'],
      ['keydown', 'KeyA'],
      ['keyup', 'KeyA'],
      ['keydown', 'KeyB'],
      ['keyup', 'KeyB'],
      ['keyup', 'ControlLeft'],
    ]) {
      window.dispatchEvent(new KeyboardEvent(type, { code, bubbles: true }));
    }
    expect(await screen.findByRole('alert')).toHaveTextContent('Press the keys together');
    expect(onChange).not.toHaveBeenCalled();
  });
  it('renders legacy shortcuts and distinguishes native modifier sides', () => {
    expect(shortcutLabels('Alt+F12')).toEqual(['Alt', 'F12']);
    expect(
      shortcutLabels(
        JSON.stringify({ platform: 'windows', keys: [{ code: 165, label: 'Right Alt' }] }),
      ),
    ).toEqual(['Right Alt']);
  });
});
