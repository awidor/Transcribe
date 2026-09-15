import { useEffect, useRef, useState } from 'react';
import { LoaderCircle, X } from 'lucide-react';
import { api } from './api';
import type { ShortcutEvent, ShortcutKey } from './types';

function keyLabel(label: string, platform?: string) {
  if (platform !== 'wayland') return label;
  return label
    .replace(/Control(Left|Right)/, '$1 Ctrl')
    .replace(/Shift(Left|Right)/, '$1 Shift')
    .replace(/Alt(Left|Right)/, '$1 Alt')
    .replace(/Meta(Left|Right)/, '$1 Super')
    .replace(/^Key/, '')
    .replace(/^Digit/, '')
    .replace(/([a-z])([A-Z])/g, '$1 $2');
}
export function shortcutLabels(value: string): string[] {
  try {
    const binding = JSON.parse(value) as { platform: string; keys: ShortcutKey[] };
    return binding.keys.map((k) => keyLabel(k.label, binding.platform));
  } catch {
    return value
      .replace('CommandOrControl', navigator.platform.includes('Mac') ? '⌘' : 'Ctrl')
      .split('+');
  }
}
interface Capture {
  token: number | null;
  platform: string;
  unlisten: (() => void) | null;
  buffered: ShortcutEvent[];
  held: Set<string>;
  keys: Map<string, ShortcutKey>;
  releasing: boolean;
  invalid: boolean;
  input: Promise<void>;
}
export function windowsKey(event: KeyboardEvent): ShortcutKey | null {
  const side =
    event.code.endsWith('Right') || event.location === KeyboardEvent.DOM_KEY_LOCATION_RIGHT ? 1 : 0;
  const modifierName = /^(Shift|Control|Alt|Meta)(Left|Right)$/.exec(event.code)?.[1] ?? event.key;
  const modifier = {
    Shift: [160, 'Shift'],
    Control: [162, 'Ctrl'],
    Alt: [164, 'Alt'],
    Meta: [91, 'Win'],
  }[modifierName] as [number, string] | undefined;
  if (modifier)
    return { code: modifier[0] + side, label: `${side ? 'Right' : 'Left'} ${modifier[1]}` };
  if (event.code === 'NumpadEnter') return { code: 269, label: 'Numpad Enter' };
  // Chromium exposes Windows virtual-key codes, including OEM/layout keys.
  if (!event.keyCode || event.keyCode === 229 || event.key === 'Unidentified') return null;
  return {
    code: event.keyCode,
    label:
      event.key === ' ' ? 'Space' : event.key.length === 1 ? event.key.toUpperCase() : event.key,
  };
}
export function ShortcutField({
  value,
  display,
  disabled,
  onChange,
  onCapturing,
}: {
  value: string;
  display?: string | null;
  disabled: boolean;
  onChange: (value: string) => void;
  onCapturing: (capturing: boolean) => void;
}) {
  const active = useRef<Capture | null>(null);
  const [mode, setMode] = useState<'idle' | 'starting' | 'listening'>('idle');
  const [keys, setKeys] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  // Capture callbacks always use the latest draft, even after another field changes.
  const callbacks = useRef({ onChange, onCapturing });
  callbacks.current = { onChange, onCapturing };

  const stop = (updateUi = true) => {
    const capture = active.current;
    active.current = null;
    capture?.unlisten?.();
    if (capture?.token != null) void api.endShortcutCapture(capture.token).catch(() => {});
    if (updateUi) {
      setMode('idle');
      callbacks.current.onCapturing(false);
    }
  };
  const receive = (capture: Capture, event: ShortcutEvent) => {
    if (active.current !== capture) return;
    if (capture.token === null) {
      capture.buffered.push(event);
      return;
    }
    if (event.token !== capture.token) return;
    if (event.kind === 'cancelled') {
      stop();
      return;
    }
    setKeys(event.keys.map((k) => keyLabel(k.label, capture.platform)));
    if (event.shortcut) {
      stop();
      callbacks.current.onChange(event.shortcut);
    }
  };
  const begin = async () => {
    if (active.current || disabled) return;
    const capture: Capture = {
      token: null,
      platform: '',
      unlisten: null,
      buffered: [],
      held: new Set(),
      keys: new Map(),
      releasing: false,
      invalid: false,
      input: Promise.resolve(),
    };
    active.current = capture;
    setMode('starting');
    setKeys([]);
    setError(null);
    callbacks.current.onCapturing(true);
    try {
      const unlisten = await api.subscribeShortcut((event) => receive(capture, event));
      if (active.current !== capture) {
        unlisten();
        return;
      }
      capture.unlisten = unlisten;
      const session = await api.beginShortcutCapture();
      if (active.current !== capture) {
        await api.endShortcutCapture(session.token);
        return;
      }
      capture.token = session.token;
      capture.platform = session.platform;
      setMode('listening');
      for (const event of capture.buffered) receive(capture, event);
      capture.buffered = [];
    } catch (e) {
      if (active.current === capture) {
        stop();
        setError(String(e));
      }
    }
  };

  useEffect(() => {
    const blur = () => stop();
    const key = (event: KeyboardEvent) => {
      const capture = active.current;
      if (!capture) return;
      event.preventDefault();
      event.stopPropagation();
      if (capture.platform === 'windows' && capture.token !== null) {
        const key = windowsKey(event);
        if (!key || event.repeat) return;
        const token = capture.token;
        const down = event.type === 'keydown';
        // Keep down/up ordered across IPC. Native capture suppresses input before
        // it reaches the browser; the engine also tolerates duplicate events.
        capture.input = capture.input
          .then(async () => {
            if (active.current !== capture) return;
            const events = await api.captureShortcutKey(token, key, down);
            for (const event of events) receive(capture, event);
          })
          .catch((error) => {
            if (active.current !== capture) return;
            stop();
            setError(String(error));
          });
        return;
      }
      if (capture.platform !== 'wayland' || capture.token === null) return;
      if (!event.code || event.code === 'Unidentified') {
        stop();
        setError('Key is unavailable to this desktop');
        return;
      }
      if (event.type === 'keydown') {
        if (event.repeat || capture.held.has(event.code)) return;
        if (capture.releasing) capture.invalid = true;
        capture.held.add(event.code);
        capture.keys.set(event.code, { code: capture.keys.size, label: event.code });
        setKeys([...capture.keys.values()].map((k) => keyLabel(k.label, 'wayland')));
      } else {
        if (!capture.held.delete(event.code)) return;
        capture.releasing = true;
        if (!capture.held.size) {
          stop();
          if (capture.invalid) {
            setError('Press the keys together');
            return;
          }
          callbacks.current.onChange(
            JSON.stringify({ platform: 'wayland', keys: [...capture.keys.values()] }),
          );
        }
      }
    };
    window.addEventListener('blur', blur);
    window.addEventListener('keydown', key, true);
    window.addEventListener('keyup', key, true);
    return () => {
      stop(false);
      window.removeEventListener('blur', blur);
      window.removeEventListener('keydown', key, true);
      window.removeEventListener('keyup', key, true);
    };
  }, []);
  useEffect(() => {
    if (mode === 'idle') return;
    const timeout = window.setTimeout(() => stop(), 15000);
    return () => window.clearTimeout(timeout);
  }, [mode]);

  const labels = mode === 'listening' ? keys : display ? [display] : shortcutLabels(value);
  return (
    <>
      <div className={`shortcut-row ${mode !== 'idle' ? 'capturing' : ''}`}>
        <button
          id="shortcut"
          type="button"
          className="shortcut-field"
          disabled={disabled || mode !== 'idle'}
          aria-label="Shortcut"
          aria-pressed={mode !== 'idle'}
          onClick={() => void begin()}
        >
          {mode === 'starting' ? (
            <LoaderCircle size={17} className="spin" />
          ) : labels.length ? (
            labels.map((label, i) => <kbd key={i}>{label}</kbd>)
          ) : (
            <span>Press keys</span>
          )}
        </button>
        {mode !== 'idle' && (
          <button
            type="button"
            className="icon-button"
            aria-label="Cancel shortcut"
            onClick={() => stop()}
          >
            <X size={17} />
          </button>
        )}
      </div>
      {error && (
        <span role="alert" className="shortcut-error">
          {error}
        </span>
      )}
    </>
  );
}
