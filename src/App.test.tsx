import { act, render, screen, fireEvent, waitFor, cleanup, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App, Widget } from './App';
import { api } from './api';
import type { Settings, ShortcutEvent } from './types';
vi.mock('./api', () => ({
  api: {
    bootstrap: vi.fn(),
    liveBootstrap: vi.fn(async () => ({
      current: { id: '', phase: 'idle' },
      sessions: [],
      hasMetaKey: false,
      hasInceptionKey: false,
    })),
    subscribeLive: vi.fn(async () => () => {}),
    subscribe: vi.fn(async () => () => {}),
    history: vi.fn(async () => []),
    copy: vi.fn(async () => {}),
    edit: vi.fn(async () => {}),
    delete: vi.fn(async () => {}),
    save: vi.fn(async (settings: Settings) => settings),
    cleanupModels: vi.fn(async () => []),
    beginShortcutCapture: vi.fn(async () => ({ token: 1, platform: 'windows' })),
    endShortcutCapture: vi.fn(async () => {}),
    subscribeShortcut: vi.fn(async (_callback: (event: ShortcutEvent) => void) => () => {}),
    toggle: vi.fn(async () => {}),
    cancel: vi.fn(async () => {}),
    updateState: vi.fn(async () => ({ phase: 'idle', current: '0.1.3', version: null, error: null })),
    subscribeUpdate: vi.fn(async () => () => {}),
  },
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.mocked(api.cleanupModels).mockReset().mockResolvedValue([]);
});
const settings: Settings = {
  microphone: null,
  shortcut: 'CommandOrControl+Shift+Space',
  cleanupModel: 'google/gemini-3.8-flash',
  cleanupReasoningEffort: 'low',
};
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
    expect(screen.getByRole('alert')).toHaveTextContent('Error');
    expect(screen.getByRole('alert')).toHaveAccessibleName('Error. Destination changed');
    expect(screen.getByRole('button', { name: 'Dismiss' })).toBeInTheDocument();
    expect(document.querySelector('.wave')).toBeNull();
    expect(document.querySelector('time')).toBeNull();
    expect(screen.queryByText('Destination changed')).not.toBeInTheDocument();
  });
  it('copy is available in history and copies the saved transcript', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [
        { id: 'one', text: 'Hello.', createdAt: 1, seconds: 2, status: 'unverified', error: null },
      ],
      settings: { ...settings },
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
      settings: { ...settings },
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
      settings: { ...settings },
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
      settings: { ...settings },
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
        { ...settings, shortcut, shortcutLabel: null },
        null,
      ),
    );
    expect(screen.getByText('Left Ctrl')).toBeInTheDocument();
  });
  it('resets thinking level on model changes and limits choices to the catalog', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { ...settings },
      microphones: [],
      hasKey: false,
      session: { phase: 'idle', startedAt: null, error: null },
    });
    vi.mocked(api.cleanupModels).mockResolvedValueOnce([
      { id: settings.cleanupModel, name: 'Flash', reasoningEfforts: ['none', 'low', 'high'] },
      { id: 'provider/mandatory', name: 'Mandatory', reasoningEfforts: ['medium', 'high'] },
      { id: 'provider/plain', name: 'Plain', reasoningEfforts: [] },
    ]);
    render(<App />);
    const model = await screen.findByLabelText('Cleanup model');
    await waitFor(() => expect(screen.queryByText('Loading models')).not.toBeInTheDocument());
    const level = screen.getByLabelText('Thinking level');
    expect(level).toHaveValue('low');
    fireEvent.change(model, { target: { value: 'provider/mandatory' } });
    expect(level).toHaveValue('');
    expect(within(level).queryByRole('option', { name: 'None' })).not.toBeInTheDocument();
    expect(within(level).queryByRole('option', { name: 'Low' })).not.toBeInTheDocument();
    fireEvent.change(level, { target: { value: 'high' } });
    fireEvent.change(model, { target: { value: 'provider/plain' } });
    expect(level).toHaveValue('');
    expect(level).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(api.save).toHaveBeenCalledWith({
      ...settings,
      cleanupModel: 'provider/plain',
      cleanupReasoningEffort: null,
    }, null));
  });
  it('preserves custom settings and saves manual models when the catalog fails', async () => {
    const stored: Settings = {
      ...settings,
      microphone: 'USB',
      cleanupModel: 'custom/saved',
      cleanupReasoningEffort: 'max',
    };
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: stored,
      microphones: ['USB'],
      hasKey: false,
      session: { phase: 'idle', startedAt: null, error: null },
    });
    vi.mocked(api.cleanupModels).mockRejectedValue(new Error('Offline'));
    render(<App />);
    await screen.findByText('Models unavailable');
    expect(screen.getByLabelText('Cleanup model')).toHaveValue('custom/saved');
    expect(screen.getByLabelText('Thinking level')).toHaveValue('max');
    fireEvent.change(screen.getByLabelText('Cleanup model'), { target: { value: 'custom/new' } });
    expect(screen.getByLabelText('Thinking level')).toHaveValue('');
    fireEvent.change(screen.getByLabelText('Thinking level'), { target: { value: 'xhigh' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(api.save).toHaveBeenCalledWith({
      ...stored,
      cleanupModel: 'custom/new',
      cleanupReasoningEffort: 'xhigh',
    }, null));
    fireEvent.click(screen.getByRole('button', { name: 'History' }));
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }));
    await screen.findByText('Models unavailable');
    expect(screen.getByLabelText('Cleanup model')).toHaveValue('custom/new');
    expect(screen.getByLabelText('Thinking level')).toHaveValue('xhigh');
    expect(screen.getByLabelText('Microphone')).toHaveValue('USB');
  });
});
