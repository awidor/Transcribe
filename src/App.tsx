import { useEffect, useRef, useState, type ReactNode } from 'react';
import {
  ArrowUpFromLine,
  AudioLines,
  Check,
  CircleAlert,
  Copy,
  History,
  KeyRound,
  Keyboard,
  LoaderCircle,
  Mic,
  Search,
  Settings2,
  Square,
  Trash2,
  X,
} from 'lucide-react';
import { api } from './api';
import { LiveCredentials, LivePanel, useLive } from './Live';
import { ShortcutField, shortcutLabels } from './ShortcutField';
import type { Entry, Session, Settings } from './types';

const idle: Session = { phase: 'idle', startedAt: null, error: null };
const BARS = [0.4, 0.8, 0.55, 1, 0.65, 0.9, 0.4];
const defaults: Settings = { microphone: null, shortcut: 'CommandOrControl+Shift+Space' };
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
export function Widget({
  session,
  level,
  act,
}: {
  session: Session;
  level: number;
  act: (fn: () => Promise<void>) => void;
}) {
  const busy = ['starting', 'transcribing', 'inserting'].includes(session.phase);
  if (session.phase === 'error') {
    return (
      <main className="widget widget-error" onContextMenu={(e) => e.preventDefault()}>
        <span
          className="widget-failure"
          role="alert"
          aria-label={session.error ? `Error. ${session.error}` : 'Error'}
          title={session.error || 'Error'}
        >
          <CircleAlert size={19} aria-hidden="true" />
          Error
        </span>
        <button className="widget-history" onClick={() => act(api.openHistory)}>
          History
        </button>
        <IconButton label="Dismiss" onClick={() => act(api.cancel)}>
          <X size={16} />
        </IconButton>
      </main>
    );
  }
  const amplitude = session.phase === 'recording' ? level : 0;
  return (
    <main
      className={session.phase === 'done' ? 'widget leaving' : 'widget'}
      onContextMenu={(e) => e.preventDefault()}
    >
      <div className="widget-side">
        {session.phase === 'done' ? (
          <Check className="complete" aria-label="Finished" />
        ) : (
          <IconButton
            label={busy ? 'Processing' : 'Stop'}
            disabled={busy}
            onClick={() => act(api.toggle)}
          >
            {busy ? <LoaderCircle className="spin" /> : <Square size={15} fill="currentColor" />}
          </IconButton>
        )}
      </div>
      <div className="wave" aria-hidden="true">
        {BARS.map((v, i) => (
          <i key={i} style={{ height: `${3 + Math.min(1, amplitude * 8) * 24 * v}px` }} />
        ))}
      </div>
      <div className="widget-side">
        <Clock startedAt={session.startedAt} running={session.phase === 'recording'} />
        <IconButton
          label="Cancel"
          disabled={session.phase === 'inserting'}
          onClick={() => act(api.cancel)}
        >
          <X size={16} />
        </IconButton>
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
  return (
    <form
      className="preferences"
      onSubmit={async (e) => {
        e.preventDefault();
        if (capturing || busy) return;
        setBusy(true);
        setComplete(false);
        try {
          await saved(draft, key.trim() || null);
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
          <div className="setting-heading">
            <span className="setting-icon">
              <KeyRound size={18} />
            </span>
            <div>
              <label htmlFor="api-key">OpenRouter</label>
            </div>
          </div>
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
          <div className="setting-heading">
            <span className="setting-icon">
              <Mic size={18} />
            </span>
            <div>
              <label htmlFor="microphone">Microphone</label>
            </div>
          </div>
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
        <div className="setting-row shortcut-setting">
          <div className="setting-heading">
            <span className="setting-icon">
              <Keyboard size={18} />
            </span>
            <div>
              <label htmlFor="shortcut">Shortcut</label>
            </div>
          </div>
          <div className="shortcut-control">
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
            <LoaderCircle size={17} className="spin" />
          ) : complete ? (
            <Check size={17} />
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
            {copied ? <Check size={17} /> : <Copy size={17} />}
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
            <Trash2 size={17} />
          </IconButton>
        </div>
      </div>
      {entry.error && (
        <div className="entry-error" role="status">
          <CircleAlert size={15} />
          {entry.error}
        </div>
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
  const live = useLive(!widget);
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
        (n: number) => setLevel((s) => (n > s ? n : s * 0.82 + n * 0.18)),
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
  if (widget) return <Widget session={session} level={level} act={act} />;
  const filtered = entries.filter((e) =>
    e.text.toLocaleLowerCase().includes(search.toLocaleLowerCase()),
  );
  const current = entries.find((e) => e.id === selected);
  const recording = session.phase === 'recording';
  const busy = ['starting', 'transcribing', 'inserting'].includes(session.phase);
  return (
    <div className="app-shell">
      <main className="workspace">
        <header>
          <h1 className="visually-hidden">
            {page === 'history' ? 'History' : page === 'live' ? 'Live' : 'Settings'}
          </h1>
          <nav aria-label="Main navigation">
            <button
              className={page === 'history' ? 'nav-button active' : 'nav-button'}
              aria-label="History"
              title="History"
              aria-current={page === 'history' ? 'page' : undefined}
              onClick={() => setPage('history')}
            >
              <History size={15} />
              <span>History</span>
            </button>
            <button
              className={page === 'live' ? 'nav-button active' : 'nav-button'}
              aria-label="Live"
              aria-current={page === 'live' ? 'page' : undefined}
              onClick={() => setPage('live')}
            >
              <AudioLines size={15} />
              <span>Live</span>
              {live.active && <span className="live-dot on" />}
            </button>
            <button
              className={page === 'settings' ? 'nav-button active' : 'nav-button'}
              aria-label="Settings"
              title="Settings"
              aria-current={page === 'settings' ? 'page' : undefined}
              onClick={() => setPage('settings')}
            >
              <Settings2 size={15} />
              <span>Settings</span>
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
                <ArrowUpFromLine size={18} />
              </IconButton>
              <button
                className={`record-button ${recording ? 'recording' : ''}`}
                aria-label={recording ? 'Stop recording' : busy ? 'Processing' : 'Record'}
                title={settings.shortcutLabel || shortcutLabels(settings.shortcut).join(' + ')}
                disabled={busy || !hasKey || live.active}
                onClick={() => act(api.toggle)}
              >
                {busy ? (
                  <LoaderCircle size={18} className="spin" />
                ) : recording ? (
                  <Square size={14} fill="currentColor" />
                ) : (
                  <Mic size={19} />
                )}
                <span>{recording ? 'Stop' : busy ? 'Processing' : 'Record'}</span>
              </button>
              {(recording || busy) && (
                <IconButton
                  label="Cancel recording"
                  disabled={session.phase === 'inserting'}
                  onClick={() => act(api.cancel)}
                >
                  <X size={17} />
                </IconButton>
              )}
            </div>
          )}
        </header>
        {(error || session.error) && (
          <div className="error-banner" role="alert">
            <CircleAlert size={16} />
            <span>{error || session.error}</span>
            <IconButton
              label="Dismiss"
              onClick={() => {
                setError(null);
                if (session.error) act(api.cancel);
              }}
            >
              <X size={14} />
            </IconButton>
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
          </div>
        ) : (
          <div className="history-layout">
            <section className="history-list">
              <div className="search">
                <Search size={16} />
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
                    className={`entry ${selected === e.id ? 'selected' : ''}`}
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
                      {e.error ? <CircleAlert size={13} /> : <span>{Math.round(e.seconds)}s</span>}
                    </div>
                    <p>{e.text || e.error || 'Processing'}</p>
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
                  <AudioLines size={34} strokeWidth={1.5} aria-hidden="true" />
                </span>
                <h2>No transcripts yet</h2>
                {hasKey ? (
                  <button
                    className="secondary-button"
                    disabled={recording || busy || live.active}
                    onClick={() => act(api.import)}
                  >
                    <ArrowUpFromLine size={15} />
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
      </main>
    </div>
  );
}
