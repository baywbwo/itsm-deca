import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { ApiError, SESSION_RELOAD_TITLE, isSessionReloadNeeded, rateLimitText, sessionReloadText, isAbort, type Api } from '../api/client';
import type {
  ActivityItem,
  ConversationPanel,
  Note,
  AssignmentEvent,
  Catalogs,
  CatalogItem,
  RelatedWork,
  TicketDetail,
  TicketListItem,
  Workspace,
} from '../api/types';
import {
  LIMITS,
  diffProps,
  formFromProps,
  idSource,
  propertyDrafts,
  propsField,
  validateProps,
  type PropertiesError,
  type PropertiesForm,
} from '../app/detail';
import { newKey } from '../app/drafts';
import { instantLabel, num } from '../app/format';
import { Badge } from '../components/Badge';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { EmptyState, ErrorState, ForbiddenState, LoadingState } from '../components/States';
import { statusTone } from './DashboardPage';

// D13: attachments are out of scope (owner decision); the Lampiran tab is not offered. The backend
// endpoints, data and fail-closed scanner controls stay as they are.
type Tab = 'summary' | 'activity' | 'work' | 'history';
const TABS: readonly { id: Tab; label: string }[] = [
  { id: 'summary', label: 'Ringkasan' },
  { id: 'activity', label: 'Aktivitas' },
  { id: 'work', label: 'Pekerjaan Jira' },
  { id: 'history', label: 'Histori' },
];

type Load = { status: 'loading' } | { status: 'error'; error: unknown } | { status: 'ok'; detail: TicketDetail };

/**
 * C13 ticket detail: side peek (native modal dialog, 640 px) or full page, same data and the
 * same server permissions. Every save goes UI → API → DB and the screen shows what the server
 * answered; `refresh` re-reads after a status/PIC change made in another dialog.
 */
export function TicketDetailView({ api, workspace, ticketId, full, refresh, canAssign, onClose, onToggleFull, onChanged, onTransition, onAssign }: {
  api: Api;
  workspace: Workspace;
  ticketId: string;
  full: boolean;
  refresh: number;
  canAssign: boolean;
  onClose: () => void;
  onToggleFull: () => void;
  onChanged: (t: TicketListItem) => void;
  onTransition: (t: TicketListItem) => void;
  onAssign: (t: TicketListItem) => void;
}) {
  const [load, setLoad] = useState<Load>({ status: 'loading' });
  const [attempt, setAttempt] = useState(0);
  const [tab, setTab] = useState<Tab>('summary');

  useEffect(() => {
    const ctl = new AbortController();
    setLoad({ status: 'loading' });
    api.getTicketDetail(workspace.id, ticketId, ctl.signal).then(
      (detail) => setLoad({ status: 'ok', detail }),
      (error: unknown) => { if (!isAbort(error)) setLoad({ status: 'error', error }); },
    );
    return () => ctl.abort();
  }, [api, workspace.id, ticketId, attempt, refresh]);

  const detail = load.status === 'ok' ? load.detail : null;
  const t = detail?.ticket ?? null;
  const applied = (d: TicketDetail) => {
    setLoad({ status: 'ok', detail: d });
    onChanged(d.ticket);
  };
  const title = t ? `${t.displayNumber} · ${t.subject}` : 'Detail tiket';

  const body = (
    <>
      {load.status === 'loading' && <LoadingState label="Memuat detail tiket…" />}
      {load.status === 'error' && <DetailError error={load.error} onRetry={() => setAttempt((n) => n + 1)} />}
      {detail && t && (
        <>
          <p className="tiny muted">{t.displayNumber} · Perusahaan pelapor {workspace.code}</p>
          <div className="row detail-actions">
            <Badge tone={statusTone(t.status)}>{t.status.label}</Badge>
            {t.priority && <Badge tone={t.priority.label.toLowerCase().includes('tinggi') ? 'error' : 'neutral'}>{t.priority.label}</Badge>}
            {!t.status.archived && t.allowedActions.length > 0 && (
              <Button onClick={() => onTransition(t)}>Ubah status</Button>
            )}
            {canAssign && !t.status.archived && <Button onClick={() => onAssign(t)}>Ubah PIC</Button>}
            <Button variant="ghost" onClick={onToggleFull}>{full ? '↙ Side peek' : '↗ Halaman penuh'}</Button>
          </div>
          {detail.can.readOnly && (
            <p className="banner" role="note">Tiket ditutup adalah arsip historis read-only. Tidak ada perubahan yang dapat disimpan.</p>
          )}
          <dl className="props">
            <Prop k="PIC operasional">{t.owner?.label ?? 'Belum ditugaskan'}</Prop>
            <Prop k="Kanal penerimaan">{detail.properties.intakeChannel?.label ?? '—'}</Prop>
            <Prop k="Kategori">{detail.properties.category?.label ?? '—'}</Prop>
            <Prop k="Merchant">{detail.properties.merchant?.label ?? '—'} <Badge tone="waiting">CIF ditunda</Badge></Prop>
            <Prop k="Produk/instrumen">{detail.properties.product?.label ?? '—'}</Prop>
            <Prop k="Unit penerima">{detail.properties.receivingUnit?.label ?? '—'}</Prop>
            <Prop k="Wilayah">{detail.properties.region?.label ?? 'Belum dipetakan'}</Prop>
            <Prop k="Tanggal penerimaan">{instantLabel(detail.properties.receivedAt, workspace.timezone)}</Prop>
            <Prop k="Siklus">{`ke-${t.cycleNo} · versi ${t.version}`}</Prop>
          </dl>
          <DetailTabs tab={tab} onChange={setTab} panelId={`detail-panel-${ticketId}`} />
          <div id={`detail-panel-${ticketId}`} role="tabpanel" aria-labelledby={`detail-tab-${tab}`} className="detail-panel">
            {tab === 'summary' && <Summary api={api} workspace={workspace} detail={detail} onSaved={applied} onReload={() => setAttempt((n) => n + 1)} />}
            {tab === 'summary' && <Conversations api={api} workspace={workspace} ticketId={t.id} />}
            {tab === 'activity' && <FollowUp api={api} workspace={workspace} detail={detail} />}
            {tab === 'work' && <Work api={api} workspace={workspace} detail={detail} onSaved={applied} onReload={() => setAttempt((n) => n + 1)} />}
            {tab === 'history' && <History api={api} workspace={workspace} detail={detail} />}
          </div>
        </>
      )}
    </>
  );

  if (full) {
    return (
      <section className="detail-page" aria-labelledby="detail-page-title">
        <div className="row between">
          <Button variant="ghost" onClick={onClose}>← Kembali ke daftar</Button>
        </div>
        <h1 id="detail-page-title" className="detail-title">{title}</h1>
        {body}
      </section>
    );
  }
  return (
    <Dialog open variant="peek" onClose={onClose} title={title}>
      {body}
    </Dialog>
  );
}

function Prop({ k, children }: { k: string; children: ReactNode }) {
  return <div className="props-row"><dt>{k}</dt><dd>{children}</dd></div>;
}

function DetailError({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  if (error instanceof ApiError && error.status === 404) {
    return <EmptyState title="Tiket tidak ditemukan" body="Tiket ini tidak ada atau tidak dapat Anda akses di workspace aktif." />;
  }
  if (error instanceof ApiError && error.status === 403) return <ForbiddenState />;
  return <ErrorState title="Detail belum dapat dimuat" body={`${message(error)} Tidak ada data lama yang dipakai.`}
    requestId={error instanceof ApiError ? error.requestId : null} onRetry={onRetry} />;
}

/** Tablist with roving focus (arrow keys, Home/End), as the List/Board tabs. */
function DetailTabs({ tab, onChange, panelId }: { tab: Tab; onChange: (t: Tab) => void; panelId: string }) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const onKey = (e: KeyboardEvent, i: number) => {
    const delta = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    const next = delta ? (i + delta + TABS.length) % TABS.length : e.key === 'Home' ? 0 : e.key === 'End' ? TABS.length - 1 : -1;
    if (next < 0) return;
    e.preventDefault();
    refs.current[next]?.focus();
    onChange(TABS[next]!.id);
  };
  return (
    <div className="tabs detail-tabs" role="tablist" aria-label="Bagian detail tiket">
      {TABS.map((t, i) => (
        <button key={t.id} ref={(el) => { refs.current[i] = el; }} type="button" role="tab" id={`detail-tab-${t.id}`}
          aria-selected={tab === t.id} aria-controls={panelId} tabIndex={tab === t.id ? 0 : -1}
          className={tab === t.id ? 'active' : undefined} onClick={() => onChange(t.id)} onKeyDown={(e) => onKey(e, i)}>
          {t.label}
        </button>
      ))}
    </div>
  );
}

type SaveError = { title: string; body: string; conflict: boolean };

function Summary({ api, workspace, detail, onSaved, onReload }: {
  api: Api;
  workspace: Workspace;
  detail: TicketDetail;
  onSaved: (d: TicketDetail) => void;
  onReload: () => void;
}) {
  const id = useId();
  const t = detail.ticket;
  const p = detail.properties;
  const saved = propertyDrafts.get(workspace.id, t.id);
  const [editing, setEditing] = useState(!!saved && detail.can.editProperties);
  const [restored] = useState(!!saved);
  const [form, setForm] = useState<PropertiesForm>(saved?.form ?? formFromProps(p));
  const [version, setVersion] = useState(saved?.version ?? t.version);
  const [catalogs, setCatalogs] = useState<Catalogs | null>(null);
  const [errors, setErrors] = useState<PropertiesError[]>([]);
  const [error, setError] = useState<SaveError | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    if (!editing) return undefined;
    const ctl = new AbortController();
    api.getCatalogs(workspace.id, ctl.signal).then(setCatalogs, () => undefined);
    return () => ctl.abort();
  }, [api, workspace.id, editing]);

  // Keep an unsaved edit in memory (never storage) while the peek is closed or the user goes Back.
  useEffect(() => {
    if (!editing) return;
    if (diffProps(form, p, version)) propertyDrafts.set(workspace.id, t.id, { form, version });
    else propertyDrafts.clear(workspace.id, t.id);
  }, [editing, form, version, p, workspace.id, t.id]);

  const set = (k: keyof PropertiesForm, v: string) => {
    setForm((f) => ({ ...f, [k]: v }));
    setErrors((es) => es.filter((e) => e.field !== k));
  };
  const fieldError = (k: keyof PropertiesForm) => errors.find((e) => e.field === k)?.message;
  // The alert sits above a long form: move focus to it so a save error is seen, not only announced.
  const errorRef = useRef<HTMLDivElement>(null);
  useEffect(() => { if (error) errorRef.current?.focus(); }, [error]);

  function startEdit() {
    setForm(formFromProps(p));
    setVersion(t.version);
    setErrors([]);
    setError(null);
    setNotice(null);
    setEditing(true);
  }
  function discard() {
    propertyDrafts.clear(workspace.id, t.id);
    setEditing(false);
    setErrors([]);
    setError(null);
  }
  /** After a 409: read the latest version, keep every typed value, base the next save on it. */
  function reloadKeep() {
    propertyDrafts.set(workspace.id, t.id, { form, version: t.version });
    setError(null);
    onReload();
  }
  // A reload after 409 brings a newer version: adopt it as the base but keep the typed form.
  useEffect(() => {
    if (editing && !error && t.version > version && propertyDrafts.get(workspace.id, t.id)) setVersion(t.version);
  }, [t.version]); // eslint-disable-line react-hooks/exhaustive-deps

  async function save() {
    if (busy) return;
    const local = validateProps(form);
    if (local.length) {
      setErrors(local);
      document.getElementById(`${id}-${local[0]!.field}`)?.focus();
      return;
    }
    const body = diffProps(form, p, version);
    if (!body) { discard(); setNotice('Tidak ada perubahan untuk disimpan.'); return; }
    setBusy(true);
    setError(null);
    try {
      const res = await api.updateTicketProperties(workspace.id, t.id, body);
      propertyDrafts.clear(workspace.id, t.id);
      setEditing(false);
      setBusy(false);
      setNotice(`Properti tersimpan (versi ${res.detail.ticket.version}) dan tercatat di audit. Tampilan dimuat dari jawaban server.`);
      onSaved(res.detail);
    } catch (e) {
      setBusy(false);
      const field = e instanceof ApiError ? propsField(e.field) : null;
      if (field) {
        setErrors([{ field, message: e instanceof ApiError ? e.message : 'Tidak valid.' }]);
        document.getElementById(`${id}-${field}`)?.focus();
      } else {
        setError(saveError(e, 'Properti'));
      }
    }
  }

  if (!editing) {
    return (
      <div>
        {notice && <p className="notice" role="status">{notice}</p>}
        <div className="row between">
          <h3>Kronologi pengaduan</h3>
          {detail.can.editProperties && <Button onClick={startEdit}>Edit properti</Button>}
        </div>
        <p className="prewrap">{p.chronology}</p>
        <div className="sectionline">
          <h3>Transaksi dan produk</h3>
          <p className={p.transactionActivity ? 'prewrap' : 'muted'}>{p.transactionActivity ?? 'Belum diisi.'}</p>
          <p className="tiny muted">Referensi cabang luar negeri: {p.foreignBranchReference ?? '—'}</p>
        </div>
        <div className="sectionline">
          <h3>Penyebab dan hasil penanganan</h3>
          <p className={p.cause ? 'prewrap' : 'muted'}>{p.cause ?? 'Belum ada hasil investigasi. Root cause tidak dipaksa saat intake.'}</p>
        </div>
        <div className="banner">
          <h3>Konteks pelanggan dan percakapan</h3>
          <p className="tiny">Identitas dari API CIF dan percakapan omnichannel belum tersambung. Nomor identitas tidak ditampilkan dan tidak perlu diketik ulang. Tidak ada pesan yang dikirim dari aplikasi ini.</p>
        </div>
      </div>
    );
  }

  const opts = (k: 'priorityId' | 'categoryId' | 'productId' | 'receivingUnitId' | 'regionId', list: CatalogItem[] | undefined) => {
    const current = p[idSource(k)] as { id: string; label: string } | null;
    const items = list ?? [];
    return (
      <>
        <option value="">— Kosong —</option>
        {current && !items.some((c) => c.id === current.id && c.active) && <option value={current.id}>{current.label} (nilai saat ini)</option>}
        {items.filter((c) => c.active).map((c) => <option key={c.id} value={c.id}>{c.label}</option>)}
      </>
    );
  };
  const select = (k: 'priorityId' | 'categoryId' | 'productId' | 'receivingUnitId' | 'regionId', label: string, list: CatalogItem[] | undefined) => (
    <div className="field">
      <label htmlFor={`${id}-${k}`}>{label}</label>
      <select id={`${id}-${k}`} className="field-control" value={form[k]} disabled={busy}
        aria-invalid={fieldError(k) ? true : undefined} onChange={(e) => set(k, e.target.value)}>
        {opts(k, list)}
      </select>
      {fieldError(k) && <span className="field-error">{fieldError(k)}</span>}
    </div>
  );
  const text = (k: keyof PropertiesForm, label: string, max: number, area = false, required = false) => (
    <div className="field full">
      <label htmlFor={`${id}-${k}`}>{label}{required ? ' *' : ''}</label>
      {area
        ? <textarea id={`${id}-${k}`} className="field-control" value={form[k]} maxLength={max} disabled={busy}
          aria-invalid={fieldError(k) ? true : undefined} aria-describedby={fieldError(k) ? `${id}-${k}-err` : undefined}
          onChange={(e) => set(k, e.target.value)} />
        : <input id={`${id}-${k}`} className="field-control" value={form[k]} maxLength={max} disabled={busy}
          aria-invalid={fieldError(k) ? true : undefined} aria-describedby={fieldError(k) ? `${id}-${k}-err` : undefined}
          onChange={(e) => set(k, e.target.value)} />}
      {fieldError(k) && <span id={`${id}-${k}-err`} className="field-error">{fieldError(k)}</span>}
    </div>
  );

  return (
    <form className="formgrid" aria-label="Edit properti tiket" onSubmit={(e) => { e.preventDefault(); void save(); }}>
      {restored && <p className="full notice" role="status">Draf properti yang belum tersimpan dipulihkan dari memori tab ini.</p>}
      {error && (
        <div className="full state-error" role="alert" ref={errorRef} tabIndex={-1}>
          <h3>{error.title}</h3>
          <p>{error.body}</p>
          {error.conflict && <Button onClick={reloadKeep}>Muat versi terbaru (isian tetap)</Button>}
        </div>
      )}
      {text('subject', 'Subjek', LIMITS.subject, false, true)}
      {text('chronology', 'Kronologi', LIMITS.chronology, true, true)}
      {select('priorityId', 'Prioritas', catalogs?.priorities)}
      {select('categoryId', 'Kategori', catalogs?.categories)}
      {select('productId', 'Produk/instrumen', catalogs?.products)}
      {select('receivingUnitId', 'Unit penerima', catalogs?.receivingUnits)}
      {select('regionId', 'Wilayah', catalogs?.regions)}
      {text('transactionActivity', 'Aktivitas transaksi', LIMITS.transactionActivity, true)}
      {text('cause', 'Penyebab dan hasil penanganan', LIMITS.cause, true)}
      {text('foreignBranchReference', 'Referensi cabang luar negeri', LIMITS.foreignBranchReference)}
      <p className="full tiny muted">Kanal, merchant dan tanggal penerimaan belum dapat diubah di sini. Menyimpan memakai versi {version}; bila tiket diubah pihak lain, server menolak (409) dan isian tetap.</p>
      <div className="full row">
        <Button onClick={discard} disabled={busy}>Buang perubahan</Button>
        <Button variant="primary" type="submit" disabled={busy} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan properti'}</Button>
      </div>
    </form>
  );
}

function Activity({ api, workspace, ticketId, version, refresh }: { api: Api; workspace: Workspace; ticketId: string; version: number; refresh: number }) {
  const [items, setItems] = useState<ActivityItem[] | null>(null);
  const [next, setNext] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const ctl = useRef<AbortController | null>(null);

  function page(cursor: string | null) {
    ctl.current?.abort();
    const c = new AbortController();
    ctl.current = c;
    setBusy(true);
    setError(null);
    api.listTicketActivity(workspace.id, ticketId, cursor, c.signal).then((r) => {
      setItems((old) => (cursor && old ? [...old, ...r.items] : r.items));
      setNext(r.nextCursor);
      setBusy(false);
    }, (e: unknown) => { if (!isAbort(e)) { setError(e); setBusy(false); } });
  }
  useEffect(() => { page(null); return () => ctl.current?.abort(); }, [workspace.id, ticketId, version, refresh]); // eslint-disable-line react-hooks/exhaustive-deps

  if (error && !items) return <ErrorState title="Aktivitas belum dapat dimuat" body={message(error)} onRetry={() => page(null)} />;
  if (!items) return <LoadingState label="Memuat aktivitas…" rows={3} />;
  if (!items.length) return <p className="muted">Belum ada aktivitas tercatat.</p>;
  return (
    <div>
      <p className="tiny muted">Dari jejak audit, terbaru di atas. Bukan chat tim; isi bebas tidak ditampilkan ulang.</p>
      <ol className="plain">
        {items.map((a) => (
          <li key={a.id} className="activity">
            <strong>{a.label}</strong>
            <span className="tiny muted"> · {instantLabel(a.at, workspace.timezone)}{a.actor ? ` · oleh ${a.actor.label}` : ''}</span>
          </li>
        ))}
      </ol>
      {error ? <p className="field-error" role="alert">{message(error)}</p> : null}
      {next && <Button onClick={() => page(next)} disabled={busy} aria-busy={busy}>{busy ? 'Memuat…' : 'Muat aktivitas lebih lama'}</Button>}
    </div>
  );
}

function Work({ api, workspace, detail, onSaved, onReload }: {
  api: Api;
  workspace: Workspace;
  detail: TicketDetail;
  onSaved: (d: TicketDetail) => void;
  onReload: () => void;
}) {
  const id = useId();
  const t = detail.ticket;
  const [adding, setAdding] = useState(false);
  const [title, setTitle] = useState('');
  const [desc, setDesc] = useState('');
  const [blocking, setBlocking] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<SaveError | null>(null);
  const [fieldErr, setFieldErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // One key per opened form: a resend after a lost answer replays instead of creating twice.
  const key = useRef(newKey());
  const resolvedOrClosed = t.status.code === 'resolved' || t.status.archived;

  async function add() {
    if (busy) return;
    if (!title.trim()) { setFieldErr('Judul pekerjaan wajib diisi.'); document.getElementById(`${id}-title`)?.focus(); return; }
    setBusy(true);
    setError(null);
    try {
      const res = await api.createRelatedWork(workspace.id, t.id, key.current, {
        title: title.trim(), safeDescription: desc.trim() || null, blocking,
      });
      setBusy(false);
      setAdding(false);
      setTitle('');
      setDesc('');
      key.current = newKey();
      setNotice(res.replayed
        ? 'Pekerjaan terkait ini sudah tersimpan sebelumnya; tidak dibuat dua kali.'
        : 'Pekerjaan terkait tersimpan dan tercatat di audit. Tidak ada issue Jira kantor yang dibuat.');
      onSaved(res.detail);
    } catch (e) {
      setBusy(false);
      setError(saveError(e, 'Pekerjaan terkait'));
    }
  }

  return (
    <div>
      {notice && <p className="notice" role="status">{notice}</p>}
      <div className="row between">
        <h3>Pekerjaan terkait</h3>
        {detail.can.manageRelatedWork && !resolvedOrClosed && !adding && (
          <Button onClick={() => { key.current = newKey(); setAdding(true); setNotice(null); }}>＋ Tambah pekerjaan</Button>
        )}
      </div>
      <p className="tiny muted">
        {num(t.relatedWork.open)} terbuka · {num(t.relatedWork.blockingOpen)} memblokir penyelesaian. Status Jira selesai tidak otomatis menyelesaikan pekerjaan atau pengaduan; penyelesaian dicatat orang dengan ringkasan dan bukti.
      </p>
      {adding && (
        <form className="formgrid card" aria-label="Tambah pekerjaan terkait" onSubmit={(e) => { e.preventDefault(); void add(); }}>
          {error && <div className="full state-error" role="alert"><h3>{error.title}</h3><p>{error.body}</p>
            {error.conflict && <Button onClick={onReload}>Muat ulang detail</Button>}</div>}
          <div className="field full">
            <label htmlFor={`${id}-title`}>Judul pekerjaan *</label>
            <input id={`${id}-title`} className="field-control" value={title} maxLength={LIMITS.relatedTitle} disabled={busy}
              aria-invalid={fieldErr ? true : undefined} onChange={(e) => { setTitle(e.target.value); setFieldErr(null); }} />
            {fieldErr && <span className="field-error">{fieldErr}</span>}
          </div>
          <div className="field full">
            <label htmlFor={`${id}-desc`}>Deskripsi aman (tanpa identitas pelanggan)</label>
            <textarea id={`${id}-desc`} className="field-control" value={desc} maxLength={LIMITS.relatedText} disabled={busy}
              onChange={(e) => setDesc(e.target.value)} />
          </div>
          <label className="full row check">
            <input type="checkbox" checked={blocking} disabled={busy} onChange={(e) => setBlocking(e.target.checked)} />
            Memblokir penyelesaian tiket sampai dicatat selesai
          </label>
          <div className="full row">
            <Button onClick={() => { setAdding(false); setError(null); }} disabled={busy}>Batal</Button>
            <Button variant="primary" type="submit" disabled={busy} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan pekerjaan'}</Button>
          </div>
        </form>
      )}
      {detail.relatedWork.length === 0
        ? <p className="muted">Belum ada pekerjaan terkait.</p>
        : detail.relatedWork.map((w) => (
          <WorkCard key={w.id} api={api} workspace={workspace} ticket={t} work={w}
            canComplete={detail.can.manageRelatedWork} onSaved={onSaved} onReload={onReload} />
        ))}
    </div>
  );
}

function WorkCard({ api, workspace, ticket, work: w, canComplete, onSaved, onReload }: {
  api: Api;
  workspace: Workspace;
  ticket: TicketListItem;
  work: RelatedWork;
  canComplete: boolean;
  onSaved: (d: TicketDetail) => void;
  onReload: () => void;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [summary, setSummary] = useState('');
  const [evidence, setEvidence] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<SaveError | null>(null);
  const [fieldErr, setFieldErr] = useState<{ field: 'summary' | 'evidence'; message: string } | null>(null);
  const tz = workspace.timezone;

  async function complete() {
    if (busy) return;
    for (const [k, v] of [['summary', summary], ['evidence', evidence]] as const) {
      if (!v.trim()) {
        setFieldErr({ field: k, message: k === 'summary' ? 'Ringkasan wajib diisi.' : 'Bukti wajib diisi.' });
        document.getElementById(`${id}-${k}`)?.focus();
        return;
      }
    }
    setBusy(true);
    setError(null);
    try {
      const res = await api.completeRelatedWork(workspace.id, ticket.id, w.id, {
        version: w.version, summary: summary.trim(), evidence: evidence.trim(),
      });
      setBusy(false);
      setOpen(false);
      onSaved(res.detail);
    } catch (e) {
      setBusy(false);
      const f = e instanceof ApiError && (e.field === 'summary' || e.field === 'evidence') ? e.field : null;
      if (f) setFieldErr({ field: f, message: e instanceof ApiError ? e.message : '' });
      else setError(saveError(e, 'Penyelesaian pekerjaan'));
    }
  }

  return (
    <article className="card work-card" aria-labelledby={`${id}-h`}>
      <div className="row between">
        <h4 id={`${id}-h`}>{w.title}</h4>
        <span className="row">
          {w.blocking && <Badge tone={w.completionState === 'open' ? 'error' : 'neutral'}>Memblokir</Badge>}
          <Badge tone={w.completionState === 'completed' ? 'ok' : 'blue'}>{w.completionState === 'completed' ? 'Selesai' : 'Terbuka'}</Badge>
        </span>
      </div>
      {w.safeDescription && <p className="prewrap tiny">{w.safeDescription}</p>}
      {w.jira
        ? <p className="tiny">Jira {w.jira.issueKey} · status provider: {w.jira.externalStatus ?? 'belum diketahui'} <Badge tone="waiting">SIMULASI</Badge></p>
        : <p className="tiny muted">Belum ditautkan ke issue Jira.</p>}
      <p className="tiny muted">Dibuat {instantLabel(w.createdAt, tz)}{w.requestedBy ? ` oleh ${w.requestedBy.label}` : ''}</p>
      {w.completionState === 'completed' && (
        <div className="tiny">
          <p><strong>Ringkasan:</strong> {w.completionSummary}</p>
          <p><strong>Bukti:</strong> {w.completionEvidence}</p>
          <p className="muted">Dicatat selesai {w.completedAt ? instantLabel(w.completedAt, tz) : ''}{w.completedBy ? ` oleh ${w.completedBy.label}` : ''}</p>
        </div>
      )}
      {w.completionState === 'open' && canComplete && !ticket.status.archived && !open && (
        <Button onClick={() => setOpen(true)}>Catat selesai</Button>
      )}
      {open && (
        <form className="formgrid" aria-label={`Catat selesai: ${w.title}`} onSubmit={(e) => { e.preventDefault(); void complete(); }}>
          {error && <div className="full state-error" role="alert"><h3>{error.title}</h3><p>{error.body}</p>
            {error.conflict && <Button onClick={onReload}>Muat ulang detail (isian tetap)</Button>}</div>}
          {(['summary', 'evidence'] as const).map((k) => (
            <div key={k} className="field full">
              <label htmlFor={`${id}-${k}`}>{k === 'summary' ? 'Ringkasan penyelesaian *' : 'Bukti (referensi uji/rilis) *'}</label>
              <textarea id={`${id}-${k}`} className="field-control" maxLength={LIMITS.relatedText} disabled={busy}
                value={k === 'summary' ? summary : evidence} aria-invalid={fieldErr?.field === k ? true : undefined}
                onChange={(e) => { (k === 'summary' ? setSummary : setEvidence)(e.target.value); setFieldErr(null); }} />
              {fieldErr?.field === k && <span className="field-error">{fieldErr.message}</span>}
            </div>
          ))}
          <div className="full row">
            <Button onClick={() => { setOpen(false); setError(null); }} disabled={busy}>Batal</Button>
            <Button variant="primary" type="submit" disabled={busy} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan penyelesaian'}</Button>
          </div>
        </form>
      )}
    </article>
  );
}

/** Aktivitas tab: operational follow-up notes (a ticket record, not team chat) above the audit trail. */
function FollowUp({ api, workspace, detail }: { api: Api; workspace: Workspace; detail: TicketDetail }) {
  const id = useId();
  const t = detail.ticket;
  const [notes, setNotes] = useState<Note[] | null>(null);
  const [next, setNext] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<SaveError | null>(null);
  const [fieldErr, setFieldErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [audit, setAudit] = useState(0);
  // One key per draft: resending the same text after a lost answer replays (200) instead of duplicating.
  const key = useRef(newKey());
  const ctl = useRef<AbortController | null>(null);

  function page(cursor: string | null) {
    ctl.current?.abort();
    const c = new AbortController();
    ctl.current = c;
    setLoadError(null);
    api.listTicketNotes(workspace.id, t.id, cursor, c.signal).then((r) => {
      setNotes((old) => (cursor && old ? [...old, ...r.items] : r.items));
      setNext(r.nextCursor);
    }, (e: unknown) => { if (!isAbort(e)) setLoadError(e); });
  }
  useEffect(() => { page(null); return () => ctl.current?.abort(); }, [workspace.id, t.id]); // eslint-disable-line react-hooks/exhaustive-deps

  async function add() {
    if (busy) return;
    if (!text.trim()) { setFieldErr('Catatan wajib diisi.'); document.getElementById(`${id}-note`)?.focus(); return; }
    setBusy(true);
    setError(null);
    try {
      const res = await api.createTicketNote(workspace.id, t.id, key.current, { body: text.trim() });
      setBusy(false);
      setText('');
      key.current = newKey();
      setNotice(res.replayed ? 'Catatan ini sudah tersimpan sebelumnya; tidak dicatat dua kali.' : 'Catatan tersimpan dan tercatat di audit.');
      setNotes((old) => [res.note, ...(old ?? []).filter((n) => n.id !== res.note.id)]);
      setAudit((n) => n + 1);
    } catch (e) {
      setBusy(false);
      if (e instanceof ApiError && e.field === 'body') setFieldErr(e.message);
      else setError(saveError(e, 'Catatan'));
    }
  }

  const tz = workspace.timezone;
  return (
    <div>
      <h3>Tindak lanjut operasional</h3>
      <p className="tiny muted">Catatan penanganan tiket, bukan chat tim. Jangan menulis identitas atau data rahasia nasabah.</p>
      {notice && <p className="notice" role="status">{notice}</p>}
      {detail.can.addNote ? (
        <form className="formgrid" aria-label="Tambah catatan tindak lanjut" onSubmit={(e) => { e.preventDefault(); void add(); }}>
          {error && <div className="full state-error" role="alert"><h3>{error.title}</h3><p>{error.body}</p></div>}
          <div className="field full">
            <label htmlFor={`${id}-note`}>Catatan tindak lanjut *</label>
            <textarea id={`${id}-note`} className="field-control" value={text} maxLength={LIMITS.note} disabled={busy}
              aria-invalid={fieldErr ? true : undefined} aria-describedby={fieldErr ? `${id}-note-err` : undefined}
              onChange={(e) => { setText(e.target.value); setFieldErr(null); setNotice(null); }} />
            {fieldErr && <span id={`${id}-note-err`} className="field-error">{fieldErr}</span>}
          </div>
          <div className="full row">
            <Button variant="primary" type="submit" disabled={busy} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan catatan'}</Button>
          </div>
        </form>
      ) : (
        <p className="tiny muted">{detail.can.readOnly ? 'Tiket ditutup: catatan baru tidak dapat ditambahkan.' : 'Peran Anda tidak dapat menambah catatan.'}</p>
      )}
      {loadError && !notes ? <ErrorState title="Catatan belum dapat dimuat" body={message(loadError)} onRetry={() => page(null)} />
        : !notes ? <LoadingState label="Memuat catatan…" rows={2} />
          : notes.length === 0 ? <p className="muted">Belum ada catatan tindak lanjut.</p> : (
            <ol className="plain" aria-label="Catatan tindak lanjut">
              {notes.map((n) => (
                <li key={n.id} className="activity">
                  <p className="prewrap">{n.body}</p>
                  <span className="tiny muted">{instantLabel(n.createdAt, tz)} · oleh {n.author.label}</span>
                </li>
              ))}
            </ol>
          )}
      {loadError && notes ? <p className="field-error" role="alert">{message(loadError)}</p> : null}
      {next && <Button onClick={() => page(next)}>Muat catatan lebih lama</Button>}
      <h3 className="spaced">Jejak audit</h3>
      <Activity api={api} workspace={workspace} ticketId={t.id} version={t.version} refresh={audit} />
    </div>
  );
}

const DELIVERY_LABEL: Record<string, string> = {
  received: 'Diterima', queued: 'Antre (belum terkirim)', sent: 'Terkirim ke provider', delivered: 'Sampai', failed: 'Gagal', unknown: 'Belum diketahui',
};

/** Customer conversation: read-only. No omnichannel adapter exists in this build, so nothing can be sent. */
function Conversations({ api, workspace, ticketId }: { api: Api; workspace: Workspace; ticketId: string }) {
  const [panel, setPanel] = useState<ConversationPanel | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    const c = new AbortController();
    setError(null);
    api.getTicketConversations(workspace.id, ticketId, c.signal).then(setPanel, (e: unknown) => { if (!isAbort(e)) setError(e); });
    return () => c.abort();
  }, [api, workspace.id, ticketId, attempt]);
  const tz = workspace.timezone;
  return (
    <section className="spaced" aria-labelledby={`conv-${ticketId}`}>
      <h3 id={`conv-${ticketId}`}>Percakapan pelanggan</h3>
      {error ? <ErrorState title="Percakapan belum dapat dimuat" body={message(error)} onRetry={() => setAttempt((n) => n + 1)} />
        : !panel ? <LoadingState label="Memuat percakapan…" rows={1} /> : (
          <>
            {!panel.adapterConfigured && (
              <p className="banner" role="note">Kanal omnichannel belum tersambung. Percakapan hanya dapat dibaca; pesan tidak dapat dikirim dari sini.</p>
            )}
            {panel.conversations.length === 0 ? <p className="muted">Belum ada percakapan tercatat untuk tiket ini.</p>
              : panel.conversations.map((cv) => (
                <article key={cv.id} className="card" aria-label={`Percakapan ${cv.channelLabel}`}>
                  <p className="tiny">{cv.channelLabel} · utas {cv.externalThreadId} <Badge tone="waiting">SIMULASI</Badge></p>
                  <ol className="plain">
                    {cv.messages.map((m) => (
                      <li key={m.id} className="activity">
                        <strong>{m.direction === 'inbound' ? 'Pelanggan' : 'Petugas'}</strong>
                        <span className="tiny muted"> · {instantLabel(m.occurredAt, tz)} · {DELIVERY_LABEL[m.deliveryState] ?? m.deliveryState}</span>
                        <p className="prewrap tiny">{m.body}</p>
                      </li>
                    ))}
                  </ol>
                </article>
              ))}
            {panel.truncated && <p className="tiny muted">Hanya percakapan terbaru yang ditampilkan.</p>}
          </>
        )}
    </section>
  );
}

const KIND_LABEL: Record<AssignmentEvent['kind'], string> = {
  initial: 'PIC awal saat tiket dibuat',
  take_for_self: 'Diambil sendiri',
  reassign: 'Dialihkan',
};

function History({ api, workspace, detail }: { api: Api; workspace: Workspace; detail: TicketDetail }) {
  const [assign, setAssign] = useState<AssignmentEvent[] | null | 'error'>(null);
  const t = detail.ticket;
  useEffect(() => {
    const ctl = new AbortController();
    api.getAssignmentHistory(workspace.id, t.id, ctl.signal).then((h) => setAssign(h.items), (e: unknown) => {
      if (!isAbort(e)) setAssign('error');
    });
    return () => ctl.abort();
  }, [api, workspace.id, t.id, t.version]);
  const tz = workspace.timezone;
  return (
    <div>
      <h3>Resolusi dan pembukaan ulang</h3>
      {detail.resolutions.length === 0 ? <p className="muted">Belum pernah diselesaikan.</p> : (
        <ol className="plain">
          {detail.resolutions.map((r) => (
            <li key={r.id} className="activity">
              <strong>{r.action === 'resolve' ? 'Diselesaikan' : 'Dibuka ulang'} · siklus ke-{r.cycleNo}</strong>
              <span className="tiny muted"> · {instantLabel(r.at, tz)}{r.actor ? ` · oleh ${r.actor.label}` : ''}</span>
              <p className="tiny prewrap">{r.summary}</p>
            </li>
          ))}
        </ol>
      )}
      <h3 className="spaced">Riwayat PIC</h3>
      {assign === null ? <p className="muted tiny" role="status">Memuat riwayat…</p>
        : assign === 'error' ? <p className="field-error" role="alert">Riwayat PIC belum dapat dimuat.</p>
          : assign.length === 0 ? <p className="muted tiny">Belum ada perubahan PIC tercatat.</p> : (
            <ol className="plain">
              {assign.map((h) => (
                <li key={h.id} className="activity">
                  <strong>{KIND_LABEL[h.kind]}</strong>
                  <span className="tiny muted"> · {instantLabel(h.at, tz)}{h.actor ? ` · oleh ${h.actor.label}` : ''}</span>
                  <p className="tiny">{h.from?.label ?? 'Belum ditugaskan'} → {h.to?.label ?? 'Belum ditugaskan'}</p>
                  {h.reason && <p className="tiny muted">Alasan: {h.reason}</p>}
                </li>
              ))}
            </ol>
          )}
    </div>
  );
}

function saveError(e: unknown, what: string): SaveError {
  if (isSessionReloadNeeded(e)) return { title: SESSION_RELOAD_TITLE, body: sessionReloadText(e.requestId), conflict: false };
  if (e instanceof ApiError) {
    const rid = e.requestId ? ` (ID permintaan ${e.requestId})` : '';
    switch (e.status) {
      case 429:
        return { title: 'Terlalu banyak permintaan', body: `${rateLimitText(e)} ${what} tidak dikirim; isian Anda tetap di sini.${rid}`, conflict: false };
      case 409:
        if (e.code === 'idempotency_conflict') {
          return { title: 'Sudah tersimpan sebelumnya', body: `Pengiriman sebelumnya dengan isian lain sudah diterima. Muat ulang detail untuk memeriksa; isian Anda tetap di sini.${rid}`, conflict: true };
        }
        return { title: 'Sudah diubah pihak lain', body: `${what} tidak disimpan. Isian Anda tetap di sini; muat versi terbaru lalu simpan lagi.${rid}`, conflict: true };
      case 403:
        return { title: 'Tidak diizinkan', body: `Peran atau sesi Anda tidak dapat melakukan perubahan ini. Tidak ada yang berubah.${rid}`, conflict: false };
      case 404:
        return { title: 'Tidak ditemukan', body: `Data ini tidak ada atau tidak dapat Anda akses.${rid}`, conflict: false };
      case 422:
        return { title: 'Ditolak aturan proses', body: `${e.message} Tidak ada yang berubah.${rid}`, conflict: false };
      default:
        return { title: 'Belum tersimpan', body: `${message(e)} Tidak ada yang berubah; coba lagi.${rid}`, conflict: false };
    }
  }
  return { title: 'Belum tersimpan', body: 'Terjadi kesalahan tak terduga. Tidak ada yang berubah.', conflict: false };
}

function message(e: unknown): string {
  if (e instanceof ApiError) return e.code === 'rate_limited' ? rateLimitText(e) : e.code === 'network' ? 'Server tidak dapat dihubungi.' : e.message;
  return 'Terjadi kesalahan tak terduga.';
}
