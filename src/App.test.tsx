import { act, render, screen, fireEvent, waitFor, cleanup } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App, Widget } from './App';
import { api } from './api';
import type { Settings, ShortcutEvent } from './types';
vi.mock('./api', () => ({
  api: {
    bootstrap: vi.fn(),
    subscribe: vi.fn(async () => () => {}),
    history: vi.fn(async () => []),
    copy: vi.fn(async () => {}),
    edit: vi.fn(async () => {}),
    delete: vi.fn(async () => {}),
    save: vi.fn(async (settings: Settings) => settings),
    beginShortcutCapture: vi.fn(async () => ({ token: 1, platform: 'windows' })),
    endShortcutCapture: vi.fn(async () => {}),
    subscribeShortcut: vi.fn(async (_callback: (event: ShortcutEvent) => void) => () => {}),
    toggle: vi.fn(async () => {}),
    cancel: vi.fn(async () => {}),
  },
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
describe('minimal interface', () => {
  it('widget never exposes copying, including after a paste failure', () => {
    render(
      <Widget
        session={{ phase: 'error', startedAt: null, error: 'Destination changed' }}
        level={0}
        act={() => {}}
      />,
    );
    expect(screen.queryByRole('button', { name: /copy/i })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'History' })).toBeInTheDocument();
    expect(screen.queryByText('Destination changed')).not.toBeInTheDocument();
  });
  it('copy is available in history and copies the saved transcript', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [
        { id: 'one', text: 'Hello.', createdAt: 1, seconds: 2, status: 'unverified', error: null },
      ],
      settings: { microphone: null, shortcut: 'CommandOrControl+Shift+Space' },
      microphones: [],
      hasKey: true,
      session: { phase: 'idle', startedAt: null, error: null },
    });
    render(<App />);
    fireEvent.click(await screen.findByRole('button', { name: 'Copy' }));
    await waitFor(() => expect(api.copy).toHaveBeenCalledWith('one'));
  });
  it('settings have no enhanced-mode toggle or subtitle controls', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { microphone: null, shortcut: 'CommandOrControl+Shift+Space' },
      microphones: ['USB'],
      hasKey: false,
      session: { phase: 'idle', startedAt: null, error: null },
    });
    render(<App />);
    expect(await screen.findByLabelText('OpenRouter')).toBeInTheDocument();
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /copy|subtitles|export/i }),
    ).not.toBeInTheDocument();
  });
  it('opens History from the native overlay even while Settings is selected', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { microphone: null, shortcut: 'CommandOrControl+Shift+Space' },
      microphones: [],
      hasKey: false,
      session: { phase: 'idle', startedAt: null, error: null },
    });
    render(<App />);
    await screen.findByLabelText('OpenRouter');
    const openHistory = vi.mocked(api.subscribe).mock.calls.at(-1)![3];
    await act(async () => openHistory());
    expect(screen.getByRole('heading', { name: 'History' })).toBeInTheDocument();
  });
  it('disables Save while capturing and saves the completed native binding', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { microphone: null, shortcut: 'CommandOrControl+Shift+Space' },
      microphones: [],
      hasKey: false,
      session: { phase: 'idle', startedAt: null, error: null },
    });
    let receive!: (event: ShortcutEvent) => void;
    vi.mocked(api.subscribeShortcut).mockImplementation(async (callback) => {
      receive = callback;
      return () => {};
    });
    render(<App />);
    fireEvent.click(await screen.findByRole('button', { name: 'Shortcut' }));
    await screen.findByText('Press keys');
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    const keys = [{ code: 162, label: 'Left Ctrl' }];
    const shortcut = JSON.stringify({ platform: 'windows', keys });
    await act(async () => receive({ kind: 'capture', token: 1, keys, shortcut }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(api.save).toHaveBeenCalledWith(
        { microphone: null, shortcut, shortcutLabel: null },
        null,
      ),
    );
    expect(screen.getByText('Left Ctrl')).toBeInTheDocument();
  });
});
