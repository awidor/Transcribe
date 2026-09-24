import { useEffect, useRef, useState } from 'react';
import { Check, Copy, LoaderCircle, Mic, Square, Trash2, X } from 'lucide-react';
import { api } from './api';
import type { LiveBootstrap, LiveSession } from './types';

export const liveIsActive = (session?: LiveSession) =>
  !!session && ['connecting', 'listening', 'stopping'].includes(session.phase);

export function useLive(enabled: boolean) {
  const [data, setData] = useState<LiveBootstrap | null>(null);
  const [level, setLevel] = useState(0);
  const revision = useRef(0);
  const refresh = async () => {
    const before = revision.current;
    const next = await api.liveBootstrap();
    setData((old) => ({
      ...next,
      current: before === revision.current ? next.current : (old?.current ?? next.current),
    }));
  };
  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let unsubscribe = () => {};
    void (async () => {
      unsubscribe = await api.subscribeLive(
        (session) => {
          if (disposed) return;
          revision.current++;
          setData((old) => ({
            hasMetaKey: old?.hasMetaKey ?? false,
            hasInceptionKey: old?.hasInceptionKey ?? false,
            current: session,
            sessions: [session, ...(old?.sessions ?? []).filter((s) => s.id !== session.id)].filter(
              (s) => s.id,
            ),
          }));
        },
        () => {
          void refresh().catch(() => {});
        },
        (n) => {
          if (!disposed) setLevel(n);
        },
      );
      if (disposed) {
        unsubscribe();
        return;
      }
      const before = revision.current;
      const next = await api.liveBootstrap();
      if (!disposed)
        setData((old) => ({
          ...next,
          current: before === revision.current ? next.current : (old?.current ?? next.current),
        }));
    })().catch(() => {});
    return () => {
      disposed = true;
      unsubscribe();
    };
  }, [enabled]);
  return { data, level, refresh, active: liveIsActive(data?.current) };
}

export function LiveCredentials({
  data,
  refresh,
}: {
  data: LiveBootstrap | null;
  refresh: () => Promise<void>;
}) {
  const [meta, setMeta] = useState('');
  const [inception, setInception] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  return (
    <form
      className="preferences live-credentials"
      onSubmit={async (e) => {
        e.preventDefault();
        if (busy) return;
        setBusy(true);
        setMessage('');
        try {
          await api.saveLiveKeys(meta.trim() || null, inception.trim() || null);
          setMeta('');
          setInception('');
          await refresh();
          setMessage('Live API keys saved');
        } catch (error) {
          setMessage(String(error));
        } finally {
          setBusy(false);
        }
      }}
    >
      <h2>Live mode</h2>
      <p className="settings-note">
        Muse Voice Transcribe streams your microphone. Mercury 2.5 updates your notes.
      </p>
      <div className="settings-card">
        <div className="setting-row">
          <label className="setting-heading" htmlFor="meta-key">
            Meta API key
          </label>
          <input
            id="meta-key"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={meta}
            onChange={(e) => {
              setMeta(e.target.value);
              setMessage('');
            }}
            placeholder={data?.hasMetaKey ? '••••••••••••••••' : 'Meta Model API key'}
          />
        </div>
        <div className="setting-row">
          <label className="setting-heading" htmlFor="inception-key">
            Inception API key
          </label>
          <input
            id="inception-key"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={inception}
            onChange={(e) => {
              setInception(e.target.value);
              setMessage('');
            }}
            placeholder={data?.hasInceptionKey ? '••••••••••••••••' : 'Inception platform key'}
          />
        </div>
      </div>
      <div className="preferences-footer">
        <span role="status">{message}</span>
        <button className="save-button" disabled={busy || (!meta.trim() && !inception.trim())}>
          {busy ? 'Saving…' : 'Save live keys'}
        </button>
      </div>
    </form>
  );
}

function elapsed(seconds: number) {
  const n = Math.max(0, Math.floor(seconds));
  return `${Math.floor(n / 60)
    .toString()
    .padStart(2, '0')}:${(n % 60).toString().padStart(2, '0')}`;
}
export function LivePanel({
  data,
  level,
  ordinaryBusy,
  refresh,
  openSettings,
  act,
}: {
  data: LiveBootstrap | null;
  level: number;
  ordinaryBusy: boolean;
  refresh: () => Promise<void>;
  openSettings: () => void;
  act: (fn: () => Promise<void>) => void;
}) {
  const [selected, setSelected] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [copied, setCopied] = useState('');
  const [now, setNow] = useState(Date.now());
  const transcriptPane = useRef<HTMLDivElement>(null);
  const followTranscript = useRef(true);
  const active = liveIsActive(data?.current);
  const current = data?.current;
  const sessions = data
    ? [
        ...(current?.id ? [current] : []),
        ...data.sessions.filter((s) => s.id !== current?.id),
      ].sort((a, b) => b.createdAt - a.createdAt)
    : [];
  const view = sessions.find((s) => s.id === selected) ?? sessions[0];
  useEffect(() => {
    followTranscript.current = true;
  }, [view?.id]);
  useEffect(() => {
    const pane = transcriptPane.current;
    if (pane && followTranscript.current) pane.scrollTop = pane.scrollHeight;
  }, [view?.id, view?.transcript, view?.interim]);
  useEffect(() => {
    if (current?.id && liveIsActive(current)) setSelected(current.id);
  }, [current?.id, current?.phase]);
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(timer);
  }, [active]);
  const ready = data?.hasMetaKey && data.hasInceptionKey;
  const invoke = (fn: () => Promise<void>) =>
    act(async () => {
      if (pending) return;
      setPending(true);
      try {
        await fn();
      } finally {
        setPending(false);
      }
    });
  const copy = (summary: boolean) =>
    act(async () => {
      if (!view) return;
      await api.copyLive(view.id, summary);
      setCopied(`${view.id}:${summary}`);
      setTimeout(() => setCopied(''), 1500);
    });
  const labels = {
    idle: 'Ready',
    connecting: 'Connecting…',
    listening: 'Listening',
    stopping: 'Finishing…',
    done: 'Finished',
    error: 'Stopped',
    cancelled: 'Cancelled',
  };
  return (
    <section className="live-mode" aria-label="Live transcription">
      <div className="live-toolbar">
        <div className="live-status" role="status">
          <span className={`live-dot ${current?.phase === 'listening' ? 'on' : ''}`} />
          <span>{active && current ? labels[current.phase] : 'Live notes'}</span>
          {active && current && (
            <time>
              {elapsed(
                current.phase === 'listening' ? (now - current.createdAt) / 1000 : current.seconds,
              )}
            </time>
          )}
          {current?.phase === 'listening' && (
            <meter aria-label="Microphone level" min={0} max={1} value={Math.min(1, level * 6)} />
          )}
        </div>
        <div className="live-actions">
          {active ? (
            <>
              <button
                className="record-button recording"
                disabled={pending || current?.phase === 'stopping'}
                onClick={() => invoke(api.stopLive)}
              >
                {current?.phase === 'stopping' ? (
                  <LoaderCircle size={14} className="spin" />
                ) : (
                  <Square size={10} fill="currentColor" />
                )}
                Stop live
              </button>
              <button
                className="icon-button"
                aria-label="Cancel live session"
                title="Stop immediately; keep completed text"
                onClick={() => invoke(api.cancelLive)}
                disabled={pending}
              >
                <X size={15} />
              </button>
            </>
          ) : (
            <button
              className="record-button"
              disabled={!ready || ordinaryBusy || pending}
              onClick={() => invoke(api.startLive)}
            >
              <Mic size={14} />
              Start live
            </button>
          )}
        </div>
      </div>
      {!ready && (
        <div className="live-notice">
          Add your Meta and Inception API keys to start.
          <button className="secondary-button" onClick={openSettings}>
            Open Settings
          </button>
        </div>
      )}
      {ordinaryBusy && (
        <p className="live-notice">Finish your current recording before starting Live.</p>
      )}
      <div className="live-layout">
        <aside className="live-sessions" aria-label="Live sessions">
          <div className="list-heading">
            <span>Sessions</span>
            <span>{sessions.length}</span>
          </div>
          {sessions.map((s) => (
            <button
              key={s.id}
              className={`entry ${view?.id === s.id ? 'selected' : ''}`}
              aria-pressed={view?.id === s.id}
              onClick={() => setSelected(s.id)}
            >
              <div>
                <time>
                  {new Date(s.createdAt).toLocaleString(undefined, {
                    month: 'short',
                    day: 'numeric',
                    hour: '2-digit',
                    minute: '2-digit',
                  })}
                </time>
              </div>
              <p>{s.transcript || labels[s.phase]}</p>
            </button>
          ))}
          {!sessions.length && <p className="list-empty">Your live sessions appear here.</p>}
        </aside>
        {view ? (
          <div className="live-content">
            {view.error && (
              <div className="live-notice live-warning" role="alert">
                {view.error}
              </div>
            )}
            <div className="live-panes">
              <section className="live-pane" aria-label="Live transcript">
                <div className="live-pane-heading">
                  <h2>Transcript</h2>
                  <button
                    className="icon-button"
                    aria-label="Copy live transcript"
                    disabled={!view.transcript}
                    onClick={() => copy(false)}
                  >
                    {copied === `${view.id}:false` ? <Check size={14} /> : <Copy size={14} />}
                  </button>
                </div>
                <div
                  className="live-text"
                  tabIndex={0}
                  ref={transcriptPane}
                  onScroll={(event) => {
                    const pane = event.currentTarget;
                    followTranscript.current =
                      pane.scrollHeight - pane.scrollTop - pane.clientHeight < 48;
                  }}
                >
                  {view.transcript && <p>{view.transcript}</p>}
                  {view.interim && (
                    <p className="live-interim">
                      <span>Provisional</span>
                      {view.interim}
                    </p>
                  )}
                  {!view.transcript && !view.interim && (
                    <p className="live-placeholder">
                      {liveIsActive(view)
                        ? 'Words will appear as you speak.'
                        : 'No speech was transcribed.'}
                    </p>
                  )}
                </div>
              </section>
              <section className="live-pane summary-pane" aria-label="Live summary">
                <div className="live-pane-heading">
                  <h2>Summary</h2>
                  <button
                    className="icon-button"
                    aria-label="Copy live summary"
                    disabled={!view.summary}
                    onClick={() => copy(true)}
                  >
                    {copied === `${view.id}:true` ? <Check size={14} /> : <Copy size={14} />}
                  </button>
                </div>
                <div className="summary-status" role="status">
                  {view.summarizing ? (
                    <>
                      <LoaderCircle size={11} className="spin" />
                      Updating…{' '}
                    </>
                  ) : null}
                  {view.summaryUpdatedAt
                    ? `Through passage ${view.summaryRevision} · ${new Date(view.summaryUpdatedAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })}`
                    : 'Waiting for completed speech'}
                </div>
                <div className="live-text" tabIndex={0}>
                  {view.summary ? (
                    <p>{view.summary}</p>
                  ) : (
                    <p className="live-placeholder">
                      Key points, decisions, and next steps will take shape here. Updates use the
                      full transcript.
                    </p>
                  )}
                  {view.summaryError && (
                    <div className="live-warning" role="alert">
                      {view.summaryError}
                    </div>
                  )}
                  {!active &&
                    view.transcript &&
                    (view.summaryError ||
                      !view.summary ||
                      view.summaryRevision < view.revision) && (
                      <button
                        className="secondary-button"
                        onClick={() => invoke(() => api.retryLiveSummary(view.id))}
                        disabled={pending}
                      >
                        {view.summaryError ? 'Retry summary' : 'Update summary'}
                      </button>
                    )}
                </div>
                <p className="summary-footnote">
                  Provisional notes. Unfinished thoughts remain unresolved.
                </p>
              </section>
            </div>
            <div className="live-footer">
              <span>
                {labels[view.phase]} · {view.revision} completed passages · 59 min per session
              </span>
              <button
                className="icon-button"
                aria-label="Delete live session"
                disabled={liveIsActive(view)}
                onClick={() =>
                  invoke(async () => {
                    await api.deleteLive(view.id);
                    setSelected(null);
                    await refresh();
                  })
                }
              >
                <Trash2 size={14} />
              </button>
            </div>
          </div>
        ) : (
          <div className="empty live-empty">
            <Mic size={24} strokeWidth={1.5} />
            <h2>Follow along as you speak</h2>
            <p>Live transcription and a summary that evolves with the conversation.</p>
          </div>
        )}
      </div>
      <p className="live-privacy">
        Microphone only. Audio is sent to Meta and is not saved by this app. Transcripts and
        summaries are saved locally; text is sent to Inception for summaries.
      </p>
    </section>
  );
}
