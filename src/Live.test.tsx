import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';
import { LiveCredentials, LivePanel } from './Live';
import { api } from './api';
import type { LiveBootstrap, LiveSession } from './types';

vi.mock('./api', () => ({
  api: {
    bootstrap: vi.fn(async () => ({
      entries: [],
      settings: {
        microphone: null,
        shortcut: 'CommandOrControl+Shift+Space',
        cleanupModel: 'google/gemini-3.8-flash',
        cleanupReasoningEffort: 'low',
      },
      microphones: [],
      hasKey: true,
      session: { phase: 'idle', startedAt: null, error: null, insert: false },
    })),
    subscribe: vi.fn(async () => () => {}),
    history: vi.fn(async () => []),
    cleanupModels: vi.fn(async () => []),
    liveBootstrap: vi.fn(),
    subscribeLive: vi.fn(async () => () => {}),
    startLive: vi.fn(async () => {}),
    stopLive: vi.fn(async () => {}),
    cancelLive: vi.fn(async () => {}),
    copyLive: vi.fn(async () => {}),
    deleteLive: vi.fn(async () => {}),
    retryLiveSummary: vi.fn(async () => {}),
    saveLiveKeys: vi.fn(async () => {}),
    toggle: vi.fn(async () => {}),
  },
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
const session: LiveSession = {
  id: 'live-one',
  createdAt: 1,
  phase: 'listening',
  transcript: 'We should wait.',
  interim: 'Until the tests',
  revision: 1,
  summary: 'A launch is under discussion.',
  summaryRevision: 1,
  summaryUpdatedAt: 2,
  summarizing: false,
  seconds: 0,
  error: null,
  summaryError: null,
};
const data = (current: Partial<LiveSession> = {}): LiveBootstrap => ({
  current: { ...session, ...current },
  sessions: [],
  hasMetaKey: true,
  hasInceptionKey: true,
});
const props = {
  level: 0.2,
  ordinaryBusy: false,
  refresh: vi.fn(async () => {}),
  openSettings: vi.fn(),
  act: (fn: () => Promise<void>) => {
    void fn();
  },
};

describe('live mode', () => {
  it('separates live mode from ordinary recording and blocks concurrent capture', async () => {
    vi.mocked(api.liveBootstrap).mockResolvedValue(data());
    render(<App />);
    const record = await screen.findByRole('button', { name: 'Record' });
    await waitFor(() => expect(record).toBeDisabled());
    fireEvent.click(screen.getByRole('button', { name: 'Live' }));
    expect(
      within(screen.getByRole('region', { name: 'Live transcript' })).getByText('We should wait.'),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Record' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Stop live' }));
    await waitFor(() => expect(api.stopLive).toHaveBeenCalledOnce());
    expect(api.toggle).not.toHaveBeenCalled();
  });
  it('keeps the old summary during an update and replaces it as a whole', () => {
    const { rerender } = render(<LivePanel {...props} data={data()} />);
    const summary = screen.getByRole('region', { name: 'Live summary' });
    expect(within(summary).queryByText('Until the tests')).not.toBeInTheDocument();
    expect(screen.getByText('Provisional')).toBeInTheDocument();
    rerender(<LivePanel {...props} data={data({ summarizing: true })} />);
    expect(within(summary).getByText('A launch is under discussion.')).toBeInTheDocument();
    expect(within(summary).getByText(/Updating/)).toBeInTheDocument();
    rerender(
      <LivePanel
        {...props}
        data={data({
          summary: 'Decision: wait until tests pass.',
          summaryRevision: 2,
          revision: 2,
          interim: '',
        })}
      />,
    );
    expect(within(summary).getByText('Decision: wait until tests pass.')).toBeInTheDocument();
    expect(within(summary).queryByText('A launch is under discussion.')).not.toBeInTheDocument();
  });
  it('requires both native keys and saves replacements without sending blanks', async () => {
    const missing = { ...data({ id: '', phase: 'idle' }), hasInceptionKey: false };
    const { unmount } = render(<LivePanel {...props} data={missing} />);
    expect(screen.getByRole('button', { name: 'Start live' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Open Settings' }));
    expect(props.openSettings).toHaveBeenCalled();
    unmount();
    render(<LiveCredentials data={missing} refresh={props.refresh} />);
    fireEvent.change(screen.getByLabelText('Inception API key'), {
      target: { value: '  test-inception  ' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Save live keys' }));
    await waitFor(() => expect(api.saveLiveKeys).toHaveBeenCalledWith(null, 'test-inception'));
    expect(await screen.findByText('Live API keys saved')).toBeInTheDocument();
    expect(screen.getByLabelText('Inception API key')).toHaveValue('');
  });
  it('keeps saved transcript and previous summary when the final summary fails', async () => {
    render(
      <LivePanel
        {...props}
        data={data({ phase: 'done', summaryError: 'Inception credits required' })}
      />,
    );
    expect(
      within(screen.getByRole('region', { name: 'Live transcript' })).getByText('We should wait.'),
    ).toBeInTheDocument();
    expect(screen.getByText('A launch is under discussion.')).toBeInTheDocument();
    expect(screen.getByRole('alert')).toHaveTextContent('Inception credits required');
    fireEvent.click(screen.getByRole('button', { name: 'Retry summary' }));
    await waitFor(() => expect(api.retryLiveSummary).toHaveBeenCalledWith('live-one'));
    fireEvent.click(screen.getByRole('button', { name: 'Copy live summary' }));
    await waitFor(() => expect(api.copyLive).toHaveBeenCalledWith('live-one', true));
  });
  it('does not let an older bootstrap overwrite a newer live event', async () => {
    let resolve!: (v: LiveBootstrap) => void;
    vi.mocked(api.liveBootstrap).mockImplementation(
      () =>
        new Promise((r) => {
          resolve = r;
        }),
    );
    render(<App />);
    await waitFor(() => expect(api.subscribeLive).toHaveBeenCalled());
    const onSession = vi.mocked(api.subscribeLive).mock.calls.at(-1)![0];
    await act(async () => onSession({ ...session, transcript: 'Latest speech.' }));
    await act(async () => resolve(data({ id: '', phase: 'idle', transcript: '' })));
    fireEvent.click(screen.getByRole('button', { name: 'Live' }));
    expect(
      within(screen.getByRole('region', { name: 'Live transcript' })).getByText('Latest speech.'),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stop live' })).toBeInTheDocument();
  });
  it('can update notes after an interrupted session left the summary behind', async () => {
    render(
      <LivePanel
        {...props}
        data={data({ phase: 'error', revision: 2, error: 'Session interrupted.' })}
      />,
    );
    expect(screen.getByRole('alert')).toHaveTextContent('Session interrupted.');
    fireEvent.click(screen.getByRole('button', { name: 'Update summary' }));
    await waitFor(() => expect(api.retryLiveSummary).toHaveBeenCalledWith('live-one'));
  });
});
