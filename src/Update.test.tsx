import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { UpdatePanel } from './Update';
import { api } from './api';
import type { UpdateState } from './types';
vi.mock('./api', () => ({
  api: {
    checkUpdate: vi.fn(async () => {}),
    installUpdate: vi.fn(async () => {}),
  },
}));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
const state = (phase: UpdateState['phase'], version: string | null = null): UpdateState => ({
  phase,
  current: '0.1.3',
  version,
  error: phase === 'error' ? 'Offline' : null,
});
const act = (fn: () => Promise<void>) => void fn();
describe('updates', () => {
  it('is hidden where releases have no updater packages', () => {
    const { container } = render(
      <UpdatePanel state={state('unsupported')} recording={false} act={act} />,
    );
    expect(container).toBeEmptyDOMElement();
  });
  it('checks for updates', async () => {
    render(<UpdatePanel state={state('current')} recording={false} act={act} />);
    expect(screen.getByText('Version 0.1.3')).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('Up to date');
    fireEvent.click(screen.getByRole('button', { name: 'Check for updates' }));
    await waitFor(() => expect(api.checkUpdate).toHaveBeenCalled());
    expect(api.installUpdate).not.toHaveBeenCalled();
  });
  it('installs an available update unless a recording is active', async () => {
    const { rerender } = render(
      <UpdatePanel state={state('available', '0.1.4')} recording={true} act={act} />,
    );
    expect(screen.getByRole('status')).toHaveTextContent('Version 0.1.4 available');
    expect(screen.getByRole('button', { name: 'Install and restart' })).toBeDisabled();
    rerender(<UpdatePanel state={state('available', '0.1.4')} recording={false} act={act} />);
    fireEvent.click(screen.getByRole('button', { name: 'Install and restart' }));
    await waitFor(() => expect(api.installUpdate).toHaveBeenCalled());
  });
  it('disables controls while installing and reports failures', () => {
    const { rerender } = render(
      <UpdatePanel state={state('installing', '0.1.4')} recording={false} act={act} />,
    );
    expect(screen.getByRole('button', { name: 'Install and restart' })).toBeDisabled();
    rerender(<UpdatePanel state={state('error')} recording={false} act={act} />);
    expect(screen.getByRole('status')).toHaveTextContent('Update failed');
    expect(screen.getByRole('status')).toHaveAttribute('title', 'Offline');
    expect(screen.getByRole('button', { name: 'Check for updates' })).toBeEnabled();
  });
});
