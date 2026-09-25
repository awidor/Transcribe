import { act, render, screen, fireEvent, waitFor, cleanup, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App, Widget } from './App';
import { api } from './api';
import type { S1Status, Session, Settings, ShortcutEvent } from './types';
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
    s1Status: vi.fn(),
    downloadS1: vi.fn(async () => {}),
    cancelS1Download: vi.fn(async () => {}),
    subscribeS1: vi.fn(async () => () => {}),
    beginShortcutCapture: vi.fn(async () => ({ token: 1, platform: 'windows' })),
    endShortcutCapture: vi.fn(async () => {}),
    subscribeShortcut: vi.fn(async (_callback: (event: ShortcutEvent) => void) => () => {}),
    toggle: vi.fn(async () => {}),
    cancel: vi.fn(async () => {}),
    grantKeyboardAccess: vi.fn(),
    updateState: vi.fn(async () => ({
      phase: 'idle',
      current: '0.1.3',
      version: null,
      error: null,
    })),
    subscribeUpdate: vi.fn(async () => () => {}),
  },
}));
const missing: S1Status = {
  engine: { ready: false, size: 573_294_991, progress: null, error: null },
  model: { ready: false, size: 484_219_808, progress: null, error: null },
};
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.mocked(api.cleanupModels).mockReset().mockResolvedValue([]);
  vi.mocked(api.s1Status).mockReset().mockResolvedValue(missing);
});
const settings: Settings = {
  microphone: null,
  shortcut: 'CommandOrControl+Shift+Space',
  cleanupModel: 'google/gemini-3.8-flash',
  cleanupReasoningEffort: 'low',
  cleanupEngine: 'openrouter',
  cleanupStyling: 'semi-formal',
  cleanupUnloadSeconds: 300,
};
describe('minimal interface', () => {
  it('pill closes with its last content and opens afresh for the next session', () => {
    const recording: Session = { phase: 'recording', startedAt: 1, error: null, retrying: false };
    const idle: Session = { phase: 'idle', startedAt: null, error: null, retrying: false };
    const { rerender } = render(<Widget session={recording} level={0} act={() => {}} />);
    const opened = document.querySelector('.widget')!;
    expect(opened).not.toHaveClass('closing');
    rerender(<Widget session={idle} level={0} act={() => {}} />);
    expect(document.querySelector('.widget')).toBe(opened);
    expect(opened).toHaveClass('closing');
    expect(opened).toHaveAttribute('aria-hidden', 'true');
    expect(screen.getByRole('button', { name: 'Stop', hidden: true })).toBeInTheDocument();
    rerender(<Widget session={{ ...recording, startedAt: 2 }} level={0} act={() => {}} />);
    const reopened = document.querySelector('.widget')!;
    expect(reopened).not.toBe(opened);
    expect(reopened).not.toHaveClass('closing');
  });
  it('pill morphs into the transcript card in place', () => {
    const inserting: Session = { phase: 'inserting', startedAt: 1, error: null, retrying: false };
    const { rerender } = render(<Widget session={inserting} level={0} act={() => {}} />);
    const pill = document.querySelector('.widget')!;
    expect(pill).not.toHaveClass('card');
    rerender(
      <Widget
        session={{ ...inserting, phase: 'error', error: 'No editable field', transcript: 'Hi.' }}
        level={0}
        act={() => {}}
      />,
    );
    expect(document.querySelector('.widget')).toBe(pill);
    expect(pill).toHaveClass('card');
    expect(screen.getByText('Hi.').closest('.widget-content')).toHaveClass('card');
  });
  it('widget never exposes copying, including after a paste failure', () => {
    render(
      <Widget
        session={{ phase: 'error', startedAt: null, error: 'Destination changed', retrying: false }}
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
  it('widget offers an unpasted transcript for dragging instead of an error', () => {
    render(
      <Widget
        session={{
          phase: 'error',
          startedAt: null,
          error: 'Destination changed',
          retrying: false,
          transcript: 'Hello there.',
        }}
        level={0}
        act={(fn) => void fn()}
      />,
    );
    expect(screen.getByRole('alert')).toHaveTextContent('Not pasted');
    expect(screen.queryByText('Error')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /copy/i })).not.toBeInTheDocument();
    const source = screen.getByText('Hello there.').closest('[draggable="true"]')!;
    expect(source).toHaveAttribute('title', 'Destination changed');
    const setData = vi.fn();
    fireEvent.dragStart(source, {
      dataTransfer: { setData, setDragImage: vi.fn(), effectAllowed: 'all' },
    });
    expect(setData).toHaveBeenCalledWith('text/plain', 'Hello there.');
    fireEvent.dragEnd(source, { dataTransfer: { dropEffect: 'none' } });
    expect(api.cancel).not.toHaveBeenCalled();
    fireEvent.dragEnd(source, { dataTransfer: { dropEffect: 'copy' } });
    expect(api.cancel).toHaveBeenCalledOnce();
  });
  it('main window keeps a failed paste quiet and marks the saved transcript', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [
        {
          id: 'one',
          text: 'Hello.',
          createdAt: 1,
          seconds: 2,
          status: 'saved',
          error: 'Destination changed',
        },
      ],
      settings: { ...settings },
      microphones: [],
      hasKey: true,
      session: {
        phase: 'error',
        startedAt: null,
        error: 'Destination changed',
        retrying: false,
        transcript: 'Hello.',
      },
    });
    render(<App />);
    expect(await screen.findByText('Not pasted')).toHaveAttribute('title', 'Destination changed');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
  it('main window reports other failures in a dismissible toast', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { ...settings },
      microphones: [],
      hasKey: true,
      session: {
        phase: 'error',
        startedAt: null,
        error: 'Microphone unavailable',
        retrying: false,
      },
    });
    render(<App />);
    expect(await screen.findByRole('alert')).toHaveTextContent('Microphone unavailable');
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
    await waitFor(() => expect(api.cancel).toHaveBeenCalledOnce());
    expect(screen.queryByRole('button', { name: 'Grant access' })).not.toBeInTheDocument();
  });
  it('main window offers to grant Linux keyboard access from the toast', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { ...settings },
      microphones: [],
      hasKey: true,
      session: {
        phase: 'idle',
        startedAt: null,
        error: 'Keyboard access required',
        retrying: false,
      },
    });
    vi.mocked(api.grantKeyboardAccess).mockRejectedValue('Keyboard access not granted');
    render(<App />);
    fireEvent.click(await screen.findByRole('button', { name: 'Grant access' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Keyboard access not granted');
    expect(screen.queryByRole('button', { name: 'Grant access' })).not.toBeInTheDocument();
  });
  it('pill flows from recording into processing and a check in one layout', () => {
    const at = (phase: Session['phase'], retrying = false): Session => ({
      phase,
      startedAt: 1,
      error: null,
      retrying,
    });
    const { rerender } = render(<Widget session={at('recording')} level={0} act={() => {}} />);
    const layout = document.querySelector('.widget-content')!;
    expect(screen.getByRole('button', { name: 'Stop' })).toBeEnabled();
    expect(document.querySelector('.widget-clock')).not.toHaveClass('gone');
    rerender(<Widget session={at('transcribing')} level={0} act={() => {}} />);
    expect(document.querySelector('.widget-content')).toBe(layout);
    expect(screen.getByRole('status')).toHaveTextContent('Transcribing');
    expect(screen.getByRole('button', { name: 'Transcribing' })).toBeDisabled();
    expect(document.querySelector('.widget-clock')).toHaveClass('gone');
    expect(screen.queryByRole('list')).not.toBeInTheDocument();
    rerender(<Widget session={at('cleaning')} level={0} act={() => {}} />);
    expect(screen.getByRole('status')).toHaveTextContent('Cleaning up');
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeEnabled();
    rerender(<Widget session={at('cleaning', true)} level={0} act={() => {}} />);
    expect(screen.getByRole('status')).toHaveTextContent('Retrying');
    for (const phase of ['inserting', 'done'] as const) {
      rerender(<Widget session={at(phase)} level={0} act={() => {}} />);
      expect(document.querySelector('.widget-content')).toBe(layout);
      expect(screen.getByRole('status')).toHaveTextContent('Done');
      expect(screen.getByRole('button', { name: 'Done' })).toHaveClass('done');
      expect(screen.queryByRole('button', { name: 'Cancel' })).not.toBeInTheDocument();
    }
  });
  it('main window names the current stage instead of a generic processing state', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [
        { id: 'one', text: '', createdAt: 1, seconds: 2, status: 'transcribing', error: null },
      ],
      settings: { ...settings },
      microphones: [],
      hasKey: true,
      session: { phase: 'cleaning', startedAt: 1, error: null, retrying: false },
    });
    render(<App />);
    expect(await screen.findByRole('button', { name: 'Cleaning up' })).toBeDisabled();
    expect(screen.getByRole('status')).toHaveTextContent('Cleaning up');
    expect(document.querySelector('.progress-line')).toBeInTheDocument();
    expect(screen.queryByRole('list', { name: 'Progress' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /s Cleaning up$/ })).toBeInTheDocument();
    expect(screen.queryByText('Processing')).not.toBeInTheDocument();
  });
  it('copy is available in history and copies the saved transcript', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [
        { id: 'one', text: 'Hello.', createdAt: 1, seconds: 2, status: 'unverified', error: null },
      ],
      settings: { ...settings },
      microphones: [],
      hasKey: true,
      session: { phase: 'idle', startedAt: null, error: null, retrying: false },
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
      session: { phase: 'idle', startedAt: null, error: null, retrying: false },
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
      session: { phase: 'idle', startedAt: null, error: null, retrying: false },
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
      session: { phase: 'idle', startedAt: null, error: null, retrying: false },
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
      expect(api.save).toHaveBeenCalledWith({ ...settings, shortcut, shortcutLabel: null }, null),
    );
    expect(screen.getByText('Left Ctrl')).toBeInTheDocument();
  });
  it('resets thinking level on model changes and limits choices to the catalog', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { ...settings },
      microphones: [],
      hasKey: false,
      session: { phase: 'idle', startedAt: null, error: null, retrying: false },
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
    await waitFor(() =>
      expect(api.save).toHaveBeenCalledWith(
        {
          ...settings,
          cleanupModel: 'provider/plain',
          cleanupReasoningEffort: null,
        },
        null,
      ),
    );
  });
  it('S1-mini replaces the OpenRouter cleanup choices with styling only', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { ...settings },
      microphones: [],
      hasKey: false,
      session: { phase: 'idle', startedAt: null, error: null, retrying: false },
    });
    render(<App />);
    const cleanup = await screen.findByLabelText('Cleanup');
    expect(screen.queryByLabelText('Styling')).not.toBeInTheDocument();
    fireEvent.change(cleanup, { target: { value: 's1-mini' } });
    expect(screen.queryByLabelText('Cleanup model')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('Thinking level')).not.toBeInTheDocument();
    const styling = screen.getByLabelText('Styling');
    expect(styling).toHaveValue('semi-formal');
    expect(
      within(styling)
        .getAllByRole('option')
        .map((option) => option.textContent),
    ).toEqual(['Casual', 'Semi-casual', 'Semi-formal', 'Formal']);
    fireEvent.change(styling, { target: { value: 'casual' } });
    const unload = screen.getByLabelText('Unload model');
    expect(unload).toHaveDisplayValue('After 5 minutes');
    expect(
      within(unload)
        .getAllByRole('option')
        .map((option) => option.textContent),
    ).toEqual([
      'Never',
      'Immediately',
      'After 2 minutes',
      'After 5 minutes',
      'After 10 minutes',
      'After 15 minutes',
      'After 1 hour',
    ]);
    fireEvent.change(unload, { target: { value: 'null' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(api.save).toHaveBeenCalledWith(
        {
          ...settings,
          cleanupEngine: 's1-mini',
          cleanupStyling: 'casual',
          cleanupUnloadSeconds: null,
        },
        null,
      ),
    );
    expect(await screen.findByLabelText('Unload model')).toHaveDisplayValue('Never');
    fireEvent.change(await screen.findByLabelText('Cleanup'), { target: { value: 'openrouter' } });
    expect(screen.getByLabelText('Cleanup model')).toHaveValue(settings.cleanupModel);
    expect(screen.queryByLabelText('Styling')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Download/ })).not.toBeInTheDocument();
  });
  it('S1-mini downloads its engine and model as separate steps with progress', async () => {
    vi.mocked(api.bootstrap).mockResolvedValue({
      entries: [],
      settings: { ...settings, cleanupEngine: 's1-mini' },
      microphones: [],
      hasKey: false,
      session: { phase: 'idle', startedAt: null, error: null, retrying: false },
    });
    let publish!: (status: S1Status) => void;
    vi.mocked(api.subscribeS1).mockImplementation(async (handler) => {
      publish = handler;
      return () => {};
    });
    render(<App />);
    const engine = await screen.findByRole('button', { name: 'Download 573 MB' });
    expect(screen.getByRole('button', { name: 'Download 484 MB' })).toBeInTheDocument();
    fireEvent.click(engine);
    expect(api.downloadS1).toHaveBeenCalledWith('engine');
    expect(api.save).not.toHaveBeenCalled();
    await act(async () => publish({ ...missing, engine: { ...missing.engine, progress: 0.426 } }));
    expect(screen.getByRole('progressbar', { name: 'Engine' })).toHaveValue(0.426);
    expect(screen.getByText('42%')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel engine download' }));
    expect(api.cancelS1Download).toHaveBeenCalledWith('engine');
    await act(async () =>
      publish({
        engine: { ready: true, size: 0, progress: null, error: null },
        model: { ...missing.model, error: 'Download failed' },
      }),
    );
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    expect(screen.getByText('Downloaded')).toBeInTheDocument();
    expect(screen.getByRole('alert')).toHaveTextContent('Download failed');
    fireEvent.click(screen.getByRole('button', { name: 'Download 484 MB' }));
    expect(api.downloadS1).toHaveBeenCalledWith('model');
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
      session: { phase: 'idle', startedAt: null, error: null, retrying: false },
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
    await waitFor(() =>
      expect(api.save).toHaveBeenCalledWith(
        {
          ...stored,
          cleanupModel: 'custom/new',
          cleanupReasoningEffort: 'xhigh',
        },
        null,
      ),
    );
    fireEvent.click(screen.getByRole('button', { name: 'History' }));
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }));
    await screen.findByText('Models unavailable');
    expect(screen.getByLabelText('Cleanup model')).toHaveValue('custom/new');
    expect(screen.getByLabelText('Thinking level')).toHaveValue('xhigh');
    expect(screen.getByLabelText('Microphone')).toHaveValue('USB');
  });
});
