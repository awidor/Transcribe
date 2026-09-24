import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  AudioLines,
  Check,
  CircleAlert,
  Copy,
  GripVertical,
  LoaderCircle,
  Mic,
  Search,
  Square,
  Trash2,
  X,
} from 'lucide-react';
import { api } from './api';
import { LiveCredentials, LivePanel, useLive } from './Live';
import { busyPhases, finished, processing, statusLabel } from './Progress';
import { ShortcutField, shortcutLabels } from './ShortcutField';
import { UpdatePanel, useUpdate } from './Update';
import { loudness, toward } from './voice';
import type {
  CleanupEngine,
  CleanupModel,
  Entry,
  ReasoningEffort,
  S1Download,
  S1Part,
  S1Status,
  Session,
  Settings,
  Styling,
} from './types';

const idle: Session = { phase: 'idle', startedAt: null, error: null, retrying: false };
const defaults: Settings = {
  microphone: null,
  shortcut: 'CommandOrControl+Shift+Space',
  cleanupModel: 'google/gemini-3.8-flash',
  cleanupReasoningEffort: 'low',
  cleanupEngine: 'openrouter',
  cleanupStyling: 'semi-formal',
  cleanupUnloadSeconds: 300,
};
const reasoningEfforts: ReasoningEffort[] = [
  'none',
  'minimal',
  'low',
  'medium',
  'high',
  'xhigh',
  'max',
];
const reasoningLabels: Record<ReasoningEffort, string> = {
  none: 'None',
  minimal: 'Minimal',
  low: 'Low',
  medium: 'Medium',
  high: 'High',
  xhigh: 'Extra high',
  max: 'Max',
};
const unloadOptions: [number | null, string][] = [
  [null, 'Never'],
  [0, 'Immediately'],
  [120, 'After 2 minutes'],
  [300, 'After 5 minutes'],
  [600, 'After 10 minutes'],
  [900, 'After 15 minutes'],
  [3600, 'After 1 hour'],
];
const stylingLabels: Record<Styling, string> = {
  casual: 'Casual',
  'semi-casual': 'Semi-casual',
  'semi-formal': 'Semi-formal',
  formal: 'Formal',
};
// Keeps the last value on screen for `duration` after it goes away, so it can
// animate out instead of vanishing.
function usePresence<T>(value: T | null, duration: number) {
  const [last, setLast] = useState(value);
  if (value !== null && value !== last) setLast(value);
  useEffect(() => {
    if (value !== null || last === null) return;
    const timer = setTimeout(() => setLast(null), duration);
    return () => clearTimeout(timer);
  }, [value, last, duration]);
  return { shown: value ?? last, leaving: value === null && last !== null };
}
function size(bytes: number) {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`;
}
function DownloadRow({
  label,
  part,
  download,
  reveal,
}: {
  label: string;
  part: S1Part;
  download: S1Download | undefined;
  reveal: boolean;
}) {
  const id = `download-${part}`;
  const state = download?.progress != null ? 'progress' : download?.ready ? 'ready' : 'download';
  const first = useRef(state);
  return (
    <div className={reveal ? 'setting-row reveal' : 'setting-row'}>
      <span className="setting-heading" id={id}>
        {label}
      </span>
      <div
        key={state}
        className={`setting-control download${state !== first.current ? ' changed' : ''}`}
      >
        {download?.progress != null ? (
          <>
            <progress value={download.progress} max={1} aria-labelledby={id} />
            <span className="download-percent">{Math.floor(download.progress * 100)}%</span>
            <IconButton
              type="button"
              label={`Cancel ${label.toLowerCase()} download`}
              onClick={() => void api.cancelS1Download(part)}
            >
              <X size={14} />
            </IconButton>
          </>
        ) : download?.ready ? (
          <span className="download-done">
            <Check size={14} aria-hidden="true" />
            Downloaded
          </span>
        ) : (
          download && (
            <>
              <button
                type="button"
                className="secondary-button"
                aria-describedby={id}
                onClick={() => void api.downloadS1(part)}
              >
                <ArrowDownToLine size={14} aria-hidden="true" />
                Download {size(download.size)}
              </button>
              {download.error && (
                <span className="download-error" role="alert">
                  {download.error}
                </span>
              )}
            </>
          )
        )}
      </div>
    </div>
  );
}
function IconButton({
  label,
  children,
  ...props
}: { label: string; children: ReactNode } & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button aria-label={label} title={label} className="icon-button" {...props}>
      {children}
    </button>
  );
}
function Clock({ startedAt, running = true }: { startedAt: number | null; running?: boolean }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(timer);
  }, [running]);
  const seconds = startedAt ? Math.max(0, Math.floor((now - startedAt) / 1000)) : 0;
  return (
    <time>
      {Math.floor(seconds / 60)
        .toString()
        .padStart(2, '0')}
      :{(seconds % 60).toString().padStart(2, '0')}
    </time>
  );
}
// The pill keeps showing its last session while it closes, and opens afresh
// (replaying its opening animation) for every new session.
function usePill(session: Session) {
  const open = session.phase !== 'idle';
  const [pill, setPill] = useState({ session, open, opening: 0 });
  if (open && (pill.session !== session || !pill.open)) {
    setPill({ session, open, opening: pill.open ? pill.opening : pill.opening + 1 });
  } else if (!open && pill.open) {
    setPill({ ...pill, open });
  }
  return pill;
}
const ENVELOPE = [0.34, 0.5, 0.68, 0.84, 0.95, 1, 0.95, 0.84, 0.68, 0.5, 0.34];
type Motion = 'listening' | 'working' | 'resting';
// Moves the bars every frame from the microphone's normalised loudness.
function useBars(level: number, motion: Motion) {
  const bars = useRef<(HTMLElement | null)[]>([]);
  const input = useRef({ level, motion });
  input.current = { level, motion };
  useEffect(() => {
    let frame = 0;
    let last = performance.now();
    const meter = loudness();
    const heights = ENVELOPE.map(() => 0.1);
    const tick = (now: number) => {
      const dt = Math.min(0.1, (now - last) / 1000);
      const t = now / 1000;
      last = now;
      const { level, motion } = input.current;
      const loud = meter(level, dt);
      ENVELOPE.forEach((shape, i) => {
        const goal =
          motion === 'listening'
            ? 0.08 +
              0.03 * Math.sin(t * 2.2 + i * 0.8) +
              0.9 * loud * shape * (0.5 + 0.5 * Math.abs(Math.sin(t * (4.1 + i * 0.83) + i * 1.9)))
            : motion === 'working'
              ? 0.12 + 0.3 * (0.5 + 0.5 * Math.sin(t * 5.2 - i * 0.6))
              : 0.08;
        heights[i] = toward(heights[i], goal, goal > heights[i] ? 34 : 14, dt);
        const bar = bars.current[i];
        if (bar) bar.style.height = `${3 + heights[i] * 19}px`;
      });
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, []);
  return bars;
}
export function Widget({
  session: current,
  level,
  act,
}: {
  session: Session;
  level: number;
  act: (fn: () => Promise<void>) => void;
}) {
  const pill = usePill(current);
  const session = pill.session;
  const closing = !pill.open && session.phase !== 'idle';
  const card = session.phase === 'error' && !!session.transcript;
  const mode = card ? 'card' : session.phase === 'error' ? 'error' : 'session';
  const recording = session.phase === 'recording';
  const done = finished(session);
  const bars = useBars(level, closing || done ? 'resting' : recording ? 'listening' : 'working');
  let content: ReactNode;
  if (mode === 'card') {
    const transcript = session.transcript!;
    content = (
      <>
        <div
          className="widget-transcript"
          draggable
          title={session.error ?? undefined}
          onDragStart={(e) => {
            const card = e.currentTarget.closest<HTMLElement>('.widget')!;
            const box = card.getBoundingClientRect();
            e.dataTransfer.effectAllowed = 'copy';
            e.dataTransfer.setData('text/plain', transcript);
            // The whole card stays legible over any destination.
            e.dataTransfer.setDragImage(card, e.clientX - box.left, e.clientY - box.top);
          }}
          onDragEnd={(e) => {
            if (e.dataTransfer.dropEffect !== 'none') act(api.cancel);
          }}
        >
          <GripVertical size={14} aria-hidden="true" />
          <p>{transcript}</p>
        </div>
        <span className="visually-hidden" role="alert">
          Not pasted
        </span>
        <IconButton label="Dismiss" onClick={() => act(api.cancel)}>
          <X size={14} />
        </IconButton>
      </>
    );
  } else if (mode === 'error') {
    content = (
      <>
        <span
          className="widget-failure"
          role="alert"
          aria-label={session.error ? `Error. ${session.error}` : 'Error'}
          title={session.error || 'Error'}
        >
          <CircleAlert size={15} aria-hidden="true" />
          Error
        </span>
        <button className="widget-history" onClick={() => act(api.openHistory)}>
          History
        </button>
        <IconButton label="Dismiss" onClick={() => act(api.cancel)}>
          <X size={14} />
        </IconButton>
      </>
    );
  } else {
    // One layout from the first word to the finished paste: the bars follow
    // the voice, then settle into a wave while the transcript is prepared,
    // and the stop button turns into a spinner and then a check.
    const status = statusLabel(session);
    content = (
      <>
        <div className={done ? 'widget-side gone' : 'widget-side'} aria-hidden={done || undefined}>
          <IconButton label="Cancel" disabled={done} onClick={() => act(api.cancel)}>
            <X size={14} />
          </IconButton>
        </div>
        <div className="wave" aria-hidden="true">
          {ENVELOPE.map((_, i) => (
            <i
              key={i}
              ref={(bar) => {
                bars.current[i] = bar;
              }}
              style={{ '--bar': i } as React.CSSProperties}
            />
          ))}
        </div>
        <div className="widget-side">
          <span className={recording ? 'widget-clock' : 'widget-clock gone'}>
            <Clock startedAt={session.startedAt} running={recording} />
          </span>
          <button
            className={`widget-action ${done ? 'done' : recording ? 'stop' : 'busy'}`}
            aria-label={recording ? 'Stop' : (status ?? 'Starting')}
            title={recording ? 'Stop' : (status ?? undefined)}
            disabled={!recording}
            onClick={() => act(api.toggle)}
          >
            {done ? (
              <Check key="done" size={14} strokeWidth={3} />
            ) : recording ? (
              <Square key="stop" size={10} fill="currentColor" />
            ) : (
              <span key="busy" className="widget-ring" />
            )}
          </button>
        </div>
        <span className="visually-hidden" role="status">
          {status}
        </span>
      </>
    );
  }
  return (
    <main
      key={pill.opening}
      className={`widget${card ? ' card' : ''}${closing ? ' closing' : ''}`}
      aria-hidden={closing || undefined}
      onContextMenu={(e) => e.preventDefault()}
    >
      <div key={mode} className={`widget-content ${mode}`}>
        {content}
      </div>
    </main>
  );
}
function Preferences({
  settings,
  microphones,
  hasKey,
  saved,
}: {
  settings: Settings;
  microphones: string[];
  hasKey: boolean;
  saved: (s: Settings, key: string | null) => Promise<void>;
}) {
  const [draft, setDraft] = useState(settings);
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [complete, setComplete] = useState(false);
  const [capturing, setCapturing] = useState(false);
  const [models, setModels] = useState<CleanupModel[]>([]);
  const [catalogLoading, setCatalogLoading] = useState(true);
  const [catalogError, setCatalogError] = useState(false);
  const [catalogAttempt, setCatalogAttempt] = useState(0);
  const [switched, setSwitched] = useState(false);
  const reveal = switched ? 'setting-row reveal' : 'setting-row';
  const [s1, setS1] = useState<S1Status>();
  useEffect(() => {
    let disposed = false;
    let unlisten = () => {};
    void api.subscribeS1(setS1).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    void api.s1Status().then(
      (status) => !disposed && setS1(status),
      () => {},
    );
    return () => {
      disposed = true;
      unlisten();
    };
  }, []);
  useEffect(() => {
    let disposed = false;
    setCatalogLoading(true);
    setCatalogError(false);
    void api.cleanupModels().then(
      (catalog) => {
        if (disposed) return;
        setModels(catalog);
        setCatalogLoading(false);
      },
      () => {
        if (disposed) return;
        setCatalogError(true);
        setCatalogLoading(false);
      },
    );
    return () => {
      disposed = true;
    };
  }, [catalogAttempt]);
  const selectedModel = models.find((model) => model.id === draft.cleanupModel);
  const availableEfforts = selectedModel?.reasoningEfforts ?? reasoningEfforts;
  return (
    <form
      className="preferences"
      onSubmit={async (e) => {
        e.preventDefault();
        if (capturing || busy) return;
        setBusy(true);
        setComplete(false);
        try {
          await saved(
            {
              ...draft,
              cleanupReasoningEffort:
                draft.cleanupReasoningEffort &&
                availableEfforts.includes(draft.cleanupReasoningEffort)
                  ? draft.cleanupReasoningEffort
                  : null,
            },
            key.trim() || null,
          );
          setKey('');
          setComplete(true);
        } catch {
          /* The parent renders the short error. */
        } finally {
          setBusy(false);
        }
      }}
    >
      <div className="settings-card">
        <div className="setting-row">
          <label className="setting-heading" htmlFor="api-key">
            OpenRouter
          </label>
          <div className="setting-control">
            <input
              id="api-key"
              type="password"
              value={key}
              autoComplete="off"
              spellCheck={false}
              placeholder={hasKey ? '••••••••••••••••' : 'API key'}
              onChange={(e) => {
                setKey(e.target.value);
                setComplete(false);
              }}
            />
          </div>
        </div>
        <div className="setting-row">
          <label className="setting-heading" htmlFor="microphone">
            Microphone
          </label>
          <select
            id="microphone"
            value={draft.microphone ?? ''}
            onChange={(e) => {
              setDraft({ ...draft, microphone: e.target.value || null });
              setComplete(false);
            }}
          >
            <option value="">System default</option>
            {microphones.map((m, i) => (
              <option key={`${m}-${i}`} value={m}>
                {m}
              </option>
            ))}
          </select>
        </div>
        <div className="setting-row">
          <label className="setting-heading" htmlFor="cleanup">
            Cleanup
          </label>
          <select
            id="cleanup"
            value={draft.cleanupEngine}
            disabled={busy}
            onChange={(e) => {
              setDraft({ ...draft, cleanupEngine: e.target.value as CleanupEngine });
              setSwitched(true);
              setComplete(false);
            }}
          >
            <option value="openrouter">OpenRouter</option>
            <option value="s1-mini">S1-mini by Superwhisper</option>
          </select>
        </div>
        {draft.cleanupEngine === 's1-mini' ? (
          <>
            <DownloadRow label="Engine" part="engine" download={s1?.engine} reveal={switched} />
            <DownloadRow label="Model" part="model" download={s1?.model} reveal={switched} />
            <div className={reveal}>
              <label className="setting-heading" htmlFor="cleanup-styling">
                Styling
              </label>
              <select
                id="cleanup-styling"
                value={draft.cleanupStyling}
                disabled={busy}
                onChange={(e) => {
                  setDraft({ ...draft, cleanupStyling: e.target.value as Styling });
                  setComplete(false);
                }}
              >
                {(Object.keys(stylingLabels) as Styling[]).map((styling) => (
                  <option key={styling} value={styling}>
                    {stylingLabels[styling]}
                  </option>
                ))}
              </select>
            </div>
            <div className={reveal}>
              <label className="setting-heading" htmlFor="cleanup-unload">
                Unload model
              </label>
              <select
                id="cleanup-unload"
                value={String(draft.cleanupUnloadSeconds)}
                disabled={busy}
                onChange={(e) => {
                  const value = e.target.value;
                  setDraft({
                    ...draft,
                    cleanupUnloadSeconds: value === 'null' ? null : Number(value),
                  });
                  setComplete(false);
                }}
              >
                {unloadOptions.map(([seconds, label]) => (
                  <option key={label} value={String(seconds)}>
                    {label}
                  </option>
                ))}
              </select>
            </div>
          </>
        ) : (
          <>
            <div className={reveal}>
              <label className="setting-heading" htmlFor="cleanup-model">
                Cleanup model
              </label>
              <div className="setting-control">
                <input
                  id="cleanup-model"
                  list="cleanup-models"
                  value={draft.cleanupModel}
                  required
                  disabled={busy}
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(e) => {
                    const cleanupModel = e.target.value;
                    setDraft((current) => ({
                      ...current,
                      cleanupModel,
                      cleanupReasoningEffort:
                        cleanupModel === current.cleanupModel
                          ? current.cleanupReasoningEffort
                          : null,
                    }));
                    setComplete(false);
                  }}
                />
                <datalist id="cleanup-models">
                  {models.map((model) => (
                    <option key={model.id} value={model.id}>
                      {model.name}
                    </option>
                  ))}
                </datalist>
                {catalogLoading && (
                  <span className="catalog-status" role="status">
                    Loading models
                  </span>
                )}
                {catalogError && (
                  <div className="catalog-status">
                    <span role="alert">Models unavailable</span>
                    <button
                      type="button"
                      className="catalog-retry"
                      disabled={busy}
                      onClick={() => setCatalogAttempt((attempt) => attempt + 1)}
                    >
                      Retry
                    </button>
                  </div>
                )}
              </div>
            </div>
            <div className={reveal}>
              <label className="setting-heading" htmlFor="cleanup-reasoning">
                Thinking level
              </label>
              <select
                id="cleanup-reasoning"
                value={
                  draft.cleanupReasoningEffort &&
                  availableEfforts.includes(draft.cleanupReasoningEffort)
                    ? draft.cleanupReasoningEffort
                    : ''
                }
                disabled={busy || availableEfforts.length === 0}
                onChange={(e) => {
                  setDraft({
                    ...draft,
                    cleanupReasoningEffort: (e.target.value || null) as ReasoningEffort | null,
                  });
                  setComplete(false);
                }}
              >
                <option value="">Model default</option>
                {availableEfforts.map((effort) => (
                  <option key={effort} value={effort}>
                    {reasoningLabels[effort]}
                  </option>
                ))}
              </select>
            </div>
          </>
        )}
        <div className="setting-row">
          <label className="setting-heading" htmlFor="shortcut">
            Shortcut
          </label>
          <div className="setting-control">
            <ShortcutField
              value={draft.shortcut}
              display={draft.shortcutLabel}
              disabled={busy}
              onCapturing={setCapturing}
              onChange={(shortcut) => {
                setDraft((current) => ({ ...current, shortcut, shortcutLabel: null }));
                setComplete(false);
              }}
            />
          </div>
        </div>
      </div>
      <div className="preferences-footer">
        <span role="status">{complete ? 'Saved' : ''}</span>
        <button
          className="save-button"
          type="submit"
          disabled={busy || capturing}
          aria-label="Save"
        >
          {busy ? (
            <LoaderCircle size={14} className="spin" />
          ) : complete ? (
            <Check size={14} />
          ) : (
            'Save'
          )}
        </button>
      </div>
    </form>
  );
}
function Editor({
  entry,
  act,
  refresh,
}: {
  entry: Entry;
  act: (fn: () => Promise<void>) => void;
  refresh: () => Promise<void>;
}) {
  const [text, setText] = useState(entry.text);
  const [copied, setCopied] = useState(false);
  const notPasted = entry.status === 'saved' && !!entry.error;
  useEffect(() => setText(entry.text), [entry.id, entry.text]);
  const save = async () => {
    if (text !== entry.text) {
      await api.edit(entry.id, text);
      await refresh();
    }
  };
  return (
    <section className="editor">
      <div className="editor-toolbar">
        <div className="editor-heading">
          <h2>Transcript</h2>
          <time>
            {new Date(entry.createdAt).toLocaleString(undefined, {
              month: 'short',
              day: 'numeric',
              hour: '2-digit',
              minute: '2-digit',
            })}
          </time>
          {notPasted && (
            <span className="entry-status" title={entry.error!}>
              Not pasted
            </span>
          )}
        </div>
        <div>
          <IconButton
            label={copied ? 'Copied' : 'Copy'}
            disabled={!text}
            onClick={() =>
              act(async () => {
                await save();
                await api.copy(entry.id);
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              })
            }
          >
            {copied ? <Check size={15} /> : <Copy size={15} />}
          </IconButton>
          <IconButton
            label="Delete"
            onClick={() =>
              act(async () => {
                await api.delete(entry.id);
                await refresh();
              })
            }
          >
            <Trash2 size={15} />
          </IconButton>
        </div>
      </div>
      {entry.error && !notPasted && (
        <p className="entry-error" role="status">
          {entry.error}
        </p>
      )}
      <textarea
        aria-label="Transcript"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onBlur={() => act(save)}
        spellCheck
      />
      <footer className="editor-footer">
        <span>{text.trim() ? text.trim().split(/\s+/).length : 0} words</span>
        <span>{Math.round(entry.seconds)} seconds</span>
      </footer>
    </section>
  );
}
export function App({ widget = false }: { widget?: boolean }) {
  const [session, setSession] = useState<Session>(idle);
  const [level, setLevel] = useState(0);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [settings, setSettings] = useState(defaults);
  const [microphones, setMicrophones] = useState<string[]>([]);
  const [hasKey, setHasKey] = useState(false);
  const [page, setPage] = useState<'history' | 'settings' | 'live'>('history');
  const [selected, setSelected] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [error, setError] = useState<string | null>(null);
  const loaded = useRef(false);
  // History entries that arrive after the first load ease into the list.
  const seen = useRef<Set<string>>(undefined);
  const nav = useRef<HTMLElement>(null);
  const [tab, setTab] = useState<{ left: number; width: number } | null>(null);
  const live = useLive(!widget);
  const update = useUpdate(!widget);
  const refresh = async () => setEntries(await api.history());
  const act = (fn: () => Promise<void>) => {
    setError(null);
    void fn().catch((e) => setError(String(e)));
  };
  useEffect(() => {
    let disposed = false;
    let unsubscribe = () => {};
    document.body.classList.toggle('widget-body', widget);
    void api
      .subscribe(
        setSession,
        () => {
          void refresh().catch(() => {});
        },
        setLevel,
        () => setPage('history'),
      )
      .then((fn) => {
        if (disposed) fn();
        else unsubscribe = fn;
      });
    void api
      .bootstrap()
      .then((b) => {
        if (disposed) return;
        setEntries(b.entries);
        setSettings(b.settings);
        setMicrophones(b.microphones);
        setHasKey(b.hasKey);
        setSession(b.session);
        if (!b.hasKey && !loaded.current) setPage('settings');
        loaded.current = true;
      })
      .catch((e) => {
        if (!disposed) setError(String(e));
      });
    return () => {
      disposed = true;
      unsubscribe();
    };
  }, [widget]);
  useEffect(() => {
    if (entries.length && !entries.some((e) => e.id === selected)) setSelected(entries[0].id);
  }, [entries, selected]);
  useEffect(() => {
    if (loaded.current) seen.current = new Set(entries.map((e) => e.id));
  }, [entries]);
  // The highlight slides to the current tab.
  useLayoutEffect(() => {
    const active = nav.current?.querySelector<HTMLElement>('[aria-current="page"]');
    if (active) setTab({ left: active.offsetLeft, width: active.offsetWidth });
  }, [page, live.active, update?.phase, widget]);
  // A failed paste is offered in the widget and kept in history, not reported here.
  const toast = usePresence(error || (session.transcript ? null : session.error), 110);
  const working = usePresence(processing(session) && !session.error ? session : null, 200);
  if (widget) return <Widget session={session} level={level} act={act} />;
  const filtered = entries.filter((e) =>
    e.text.toLocaleLowerCase().includes(search.toLocaleLowerCase()),
  );
  const current = entries.find((e) => e.id === selected);
  const recording = session.phase === 'recording';
  const busy = busyPhases.includes(session.phase);
  const status = statusLabel(session);
  return (
    <div className="app-shell">
      <main className="workspace">
        <header>
          <h1 className="visually-hidden">
            {page === 'history' ? 'History' : page === 'live' ? 'Live' : 'Settings'}
          </h1>
          <nav aria-label="Main navigation" ref={nav}>
            {tab && (
              <span
                className="nav-indicator"
                aria-hidden="true"
                style={{ width: tab.width, transform: `translateX(${tab.left}px)` }}
              />
            )}
            <button
              className={page === 'history' ? 'nav-button active' : 'nav-button'}
              aria-label="History"
              aria-current={page === 'history' ? 'page' : undefined}
              onClick={() => setPage('history')}
            >
              History
            </button>
            <button
              className={page === 'live' ? 'nav-button active' : 'nav-button'}
              aria-label="Live"
              aria-current={page === 'live' ? 'page' : undefined}
              onClick={() => setPage('live')}
            >
              Live
              {live.active && <span className="live-dot on" />}
            </button>
            <button
              className={page === 'settings' ? 'nav-button active' : 'nav-button'}
              aria-label={update?.phase === 'available' ? 'Settings, update available' : 'Settings'}
              aria-current={page === 'settings' ? 'page' : undefined}
              onClick={() => setPage('settings')}
            >
              Settings
              {update?.phase === 'available' && <span className="live-dot on" />}
            </button>
          </nav>
          {page !== 'live' && (
            <div className="header-actions">
              {recording && <Clock startedAt={session.startedAt} />}
              <IconButton
                label="Import audio"
                disabled={busy || recording || !hasKey || live.active}
                onClick={() => act(api.import)}
              >
                <ArrowUpFromLine size={15} />
              </IconButton>
              <button
                className={`record-button ${recording ? 'recording' : ''}`}
                aria-label={recording ? 'Stop recording' : busy ? status! : 'Record'}
                title={settings.shortcutLabel || shortcutLabels(settings.shortcut).join(' + ')}
                disabled={busy || !hasKey || live.active}
                onClick={() => act(api.toggle)}
              >
                {busy && finished(session) ? (
                  <Check size={14} />
                ) : busy ? (
                  <LoaderCircle size={14} className="spin" />
                ) : recording ? (
                  <Square size={10} fill="currentColor" />
                ) : (
                  <Mic size={14} />
                )}
                <span key={recording ? 'Stop' : busy ? status : 'Record'}>
                  {recording ? 'Stop' : busy ? status : 'Record'}
                </span>
              </button>
              {(recording || (busy && !finished(session))) && (
                <IconButton label="Cancel recording" onClick={() => act(api.cancel)}>
                  <X size={15} />
                </IconButton>
              )}
            </div>
          )}
        </header>
        {working.shown && (
          <div
            className={`progress-line${finished(working.shown) ? ' done' : ''}${working.leaving ? ' leaving' : ''}`}
          >
            {!working.leaving && (
              <span className="visually-hidden" role="status">
                {status}
              </span>
            )}
          </div>
        )}
        {page === 'live' ? (
          <LivePanel
            data={live.data}
            level={live.level}
            ordinaryBusy={recording || busy}
            refresh={live.refresh}
            openSettings={() => setPage('settings')}
            act={act}
          />
        ) : page === 'settings' ? (
          <div className="settings-page">
            <Preferences
              key={JSON.stringify(settings)}
              settings={settings}
              microphones={microphones}
              hasKey={hasKey}
              saved={async (s, key) => {
                try {
                  const updated = await api.save(s, key);
                  setSettings(updated);
                  if (key) setHasKey(true);
                  setError(null);
                } catch (e) {
                  setError(String(e));
                  throw e;
                }
              }}
            />
            <LiveCredentials data={live.data} refresh={live.refresh} />
            <UpdatePanel state={update} recording={recording || busy || live.active} act={act} />
          </div>
        ) : (
          <div className="history-layout">
            <section className="history-list">
              <div className="search">
                <Search size={13} />
                <input
                  aria-label="Search history"
                  placeholder="Search transcripts"
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                />
              </div>
              <div className="list-heading">
                <span>{search ? 'Search results' : 'All transcripts'}</span>
                <span>{filtered.length}</span>
              </div>
              <div className="entries">
                {filtered.map((e) => (
                  <button
                    className={`entry${selected === e.id ? ' selected' : ''}${seen.current && !seen.current.has(e.id) ? ' fresh' : ''}`}
                    key={e.id}
                    aria-pressed={selected === e.id}
                    onClick={() => setSelected(e.id)}
                  >
                    <div>
                      <time>
                        {new Date(e.createdAt).toLocaleString(undefined, {
                          month: 'short',
                          day: 'numeric',
                          hour: '2-digit',
                          minute: '2-digit',
                        })}
                      </time>
                      {e.status === 'failed' ? (
                        <span className="entry-dot" aria-hidden="true" />
                      ) : (
                        <span>{Math.round(e.seconds)}s</span>
                      )}
                    </div>
                    <p>
                      {e.text || e.error || (e.status === 'transcribing' && status) || 'Processing'}
                    </p>
                  </button>
                ))}
                {!filtered.length && (
                  <p className="list-empty">{search ? 'No matches' : 'No transcripts'}</p>
                )}
              </div>
            </section>
            {current ? (
              <Editor key={current.id} entry={current} act={act} refresh={refresh} />
            ) : (
              <div className="empty">
                <span className="empty-icon">
                  <AudioLines size={24} strokeWidth={1.5} aria-hidden="true" />
                </span>
                <h2>No transcripts yet</h2>
                {hasKey ? (
                  <button
                    className="secondary-button"
                    disabled={recording || busy || live.active}
                    onClick={() => act(api.import)}
                  >
                    <ArrowUpFromLine size={13} />
                    Import audio
                  </button>
                ) : (
                  <button className="secondary-button" onClick={() => setPage('settings')}>
                    Open Settings
                  </button>
                )}
              </div>
            )}
          </div>
        )}
        {toast.shown && (
          <div
            className={`toast${toast.leaving ? ' leaving' : ''}`}
            role={toast.leaving ? undefined : 'alert'}
          >
            <span className="toast-dot" aria-hidden="true" />
            <span className="toast-text" title={toast.shown}>
              {toast.shown}
            </span>
            <IconButton
              label="Dismiss"
              disabled={toast.leaving}
              onClick={() => (error ? setError(null) : act(api.cancel))}
            >
              <X size={13} />
            </IconButton>
          </div>
        )}
      </main>
    </div>
  );
}
