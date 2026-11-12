import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
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
import { ageLabel, instantLabel, num } from '../app/format';
import { initials } from '../app/labels';
import { priorityDot } from '../app/ticketView';
import { Icon } from '../components/Icon';
import { Badge } from '../components/Badge';
import { Button } from '../components/Button';
import { WordingAssistant } from '../components/WordingAssistant';
import { Dialog } from '../components/Dialog';
import { EmptyState, ErrorState, ForbiddenState, LoadingState } from '../components/States';
import { statusTone } from '../components/Badge';

type Load = { status: 'loading' } | { status: 'error'; error: unknown } | { status: 'ok'; detail: TicketDetail };

/**
 * C13 ticket detail: side peek (native modal dialog, 640 px) or full page, same data and the
 * same server permissions. Every save goes UI → API → DB and the screen shows what the server
 * answered; `refresh` re-reads after a status/PIC change made in another dialog.
 * CP-UI2: owner prototype layout (header card with actions, two columns, no tabs; one column in
 * the peek and below 1080 px). D13: attachments stay out of scope (no Lampiran section).
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
  const [audit, setAudit] = useState(0);
  const [escalate, setEscalate] = useState(0);

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
  const tz = workspace.timezone;
  const reload = () => setAttempt((n) => n + 1);

  const body = (
    <>
      {load.status === 'loading' && <LoadingState label="Memuat detail tiket…" />}
      {load.status === 'error' && <DetailError error={load.error} onRetry={reload} />}
      {detail && t && (() => {
        const p = detail.properties;
        const open = !t.status.archived;
        const canEscalate = detail.can.manageRelatedWork && open && t.status.code !== 'resolved';
        return (
          <>
            <header className="dhead">
              <div className="dhead-main">
                <p className="dhead-id">
                  <span className="dno">{t.displayNumber}</span>
                  <Badge tone={statusTone(t.status)}>{t.status.label}</Badge>
                  {t.priority && <span className={`pri ${priorityDot(t.priority.code)}`}><b aria-hidden="true" />{t.priority.label}</span>}
                </p>
                {full ? <h2 className="dtitle">{t.subject}</h2> : <p className="dtitle">{t.subject}</p>}
                <p className="meta">
                  <span>Merchant <b>{p.merchant?.label ?? '—'}</b></span>
                  <span>Kanal <b>{p.intakeChannel?.label ?? '—'}</b></span>
                  <span>Diterima <b>{instantLabel(p.receivedAt, tz)}</b></span>
                  <span>Umur <b>{ageLabel(t.ageSeconds)}</b></span>
                  <span>Siklus <b>{t.cycleNo}</b></span>
                </p>
              </div>
              <div className="acts">
                {canAssign && open && <Button onClick={() => onAssign(t)}><Icon name="user" size={16} /> Penugasan</Button>}
                {canEscalate && <Button onClick={() => setEscalate((n) => n + 1)}><Icon name="chevron" size={16} /> Eskalasi</Button>}
                {open && t.allowedActions.length > 0 && (
                  <Button variant="primary" onClick={() => onTransition(t)}><Icon name="check" size={16} /> Ubah status</Button>
                )}
                <Button variant="ghost" onClick={onToggleFull}>{full ? '↙ Side peek' : '↗ Halaman penuh'}</Button>
              </div>
            </header>
            {detail.can.readOnly && (
              <p className="banner" role="note">Tiket ditutup adalah arsip historis read-only. Tidak ada perubahan yang dapat disimpan.</p>
            )}
            <div className="detail2">
              <div className="dcol">
                <Card id={`kron-${t.id}`} title="Kronologi" sub="disimpan sebagai teks bebas">
                  <Summary api={api} workspace={workspace} detail={detail} onSaved={applied} onReload={reload} />
                </Card>
                <Card id={`note-${t.id}`} title="Catatan internal" sub="tidak dikirim ke pelapor" tag="append-only">
                  <FollowUp api={api} workspace={workspace} detail={detail} onAdded={() => setAudit((n) => n + 1)} />
                </Card>
                <Card id={`cycle-${t.id}`} title="Riwayat siklus" sub="resolve & reopen">
                  <CycleTimeline workspace={workspace} detail={detail} />
                </Card>
                <Card id={`conv-${t.id}`} title="Percakapan pelanggan" sub="hanya baca">
                  <Conversations api={api} workspace={workspace} ticketId={t.id} />
                </Card>
                <Card id={`audit-${t.id}`} title="Jejak audit" sub="terbaru di atas">
                  <Activity api={api} workspace={workspace} ticketId={t.id} version={t.version} refresh={audit} />
                </Card>
              </div>
              <div className="dcol">
                <Card id={`attr-${t.id}`} title="Atribut">
                  <dl className="kv">
                    <dt>Kategori</dt><dd>{p.category?.label ?? '—'}</dd>
                    <dt>Produk</dt><dd>{p.product?.label ?? '—'}</dd>
                    <dt>Unit</dt><dd>{p.receivingUnit?.label ?? '—'}</dd>
                    <dt>Wilayah</dt><dd>{p.region?.label ?? 'Belum dipetakan'}</dd>
                    <dt>Channel</dt><dd>{p.intakeChannel?.label ?? '—'}</dd>
                    <dt>Prioritas</dt><dd>{p.priority?.label ?? '—'}</dd>
                    <dt>Merchant</dt><dd>{p.merchant?.label ?? '—'} <Badge tone="waiting">CIF ditunda</Badge></dd>
                    <dt>Diterima</dt><dd>{instantLabel(p.receivedAt, tz)}</dd>
                    <dt>Diperbarui</dt><dd>{instantLabel(t.updatedAt, tz)}</dd>
                    <dt>Versi</dt><dd>{t.version}</dd>
                  </dl>
                </Card>
                <Card id={`pic-${t.id}`} title="Penanggung jawab">
                  <Owner api={api} workspace={workspace} detail={detail} canAssign={canAssign && open} onAssign={() => onAssign(t)} />
                </Card>
                <Card id={`work-${t.id}`} title="Pekerjaan terkait" sub={`${num(detail.relatedWork.length)} item`}>
                  <Work api={api} workspace={workspace} detail={detail} onSaved={applied} onReload={reload} openSignal={escalate} />
                </Card>
              </div>
            </div>
          </>
        );
      })()}
    </>
  );

  if (full) {
    return (
      <section className="detail-page" aria-labelledby="detail-page-title">
        <div className="row between">
          <Button variant="ghost" onClick={onClose}>← Kembali ke daftar</Button>
        </div>
        <h1 id="detail-page-title" className="sr-only">{title}</h1>
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

/** Prototype `.card` with `.hd` (title, muted sub, optional tag on the right) and `.bd`. */
function Card({ id, title, sub, tag, children }: { id: string; title: string; sub?: string; tag?: string; children: ReactNode }) {
  return (
    <section className="pcard" aria-labelledby={id}>
      <header className="hd">
        <h3 id={id}>{title}</h3>
        {sub && <span className="sub">{sub}</span>}
        {tag && <span className="r"><span className="chip-tag">{tag}</span></span>}
      </header>
      <div className="bd">{children}</div>
    </section>
  );
}

function DetailError({ error, onRetry }: { error: unknown; onRetry: () => void }) {
  if (error instanceof ApiError && error.status === 404) {
    return <EmptyState title="Tiket tidak ditemukan" body="Tiket ini tidak ada atau tidak dapat Anda akses di workspace aktif." />;
  }
  if (error instanceof ApiError && error.status === 403) return <ForbiddenState />;
  return <ErrorState title="Detail belum dapat dimuat" body={`${message(error)} Tidak ada data lama yang dipakai.`}
    requestId={error instanceof ApiError ? error.requestId : null} onRetry={onRetry} />;
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
        <p className="prewrap">{p.chronology}</p>
        {detail.can.editProperties && <div className="row"><Button className="btn-small" onClick={startEdit}>Edit properti</Button></div>}
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

function Work({ api, workspace, detail, onSaved, onReload, openSignal }: {
  api: Api;
  workspace: Workspace;
  detail: TicketDetail;
  onSaved: (d: TicketDetail) => void;
  onReload: () => void;
  /** CP-UI2 "Eskalasi": each increment opens the add form and moves focus to its title field. */
  openSignal: number;
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
  useEffect(() => {
    if (!openSignal || !detail.can.manageRelatedWork || resolvedOrClosed) return;
    if (!adding) key.current = newKey();
    setAdding(true);
    setNotice(null);
    window.setTimeout(() => {
      const el = document.getElementById(`${id}-title`);
      el?.scrollIntoView({ block: 'center' });
      el?.focus();
    }, 0);
  }, [openSignal]); // eslint-disable-line react-hooks/exhaustive-deps

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
      {detail.can.manageRelatedWork && !resolvedOrClosed && !adding && (
        <div className="row"><Button className="btn-small" onClick={() => { key.current = newKey(); setAdding(true); setNotice(null); }}>＋ Tambah pekerjaan</Button></div>
      )}
      <p className="tiny muted">
        {num(t.relatedWork.open)} terbuka · {num(t.relatedWork.blockingOpen)} memblokir penyelesaian. Status Jira selesai tidak otomatis menyelesaikan pekerjaan atau pengaduan; penyelesaian dicatat orang dengan ringkasan dan bukti.
      </p>
      {adding && (
        <form className="formgrid composer" aria-label="Tambah pekerjaan terkait (eskalasi)" onSubmit={(e) => { e.preventDefault(); void add(); }}>
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
        ? <p className="muted tiny">Belum ada pekerjaan terkait.</p>
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

/** Catatan internal: append-only follow-up notes (a ticket record, not team chat). */
function FollowUp({ api, workspace, detail, onAdded }: { api: Api; workspace: Workspace; detail: TicketDetail; onAdded: () => void }) {
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
      onAdded();
    } catch (e) {
      setBusy(false);
      if (e instanceof ApiError && e.field === 'body') setFieldErr(e.message);
      else setError(saveError(e, 'Catatan'));
    }
  }

  const tz = workspace.timezone;
  return (
    <div>
      {notice && <p className="notice" role="status">{notice}</p>}
      {loadError && !notes ? <ErrorState title="Catatan belum dapat dimuat" body={message(loadError)} onRetry={() => page(null)} />
        : !notes ? <LoadingState label="Memuat catatan…" rows={2} />
          : notes.length === 0 ? <p className="muted tiny">Belum ada catatan.</p> : (
            <ol className="plain notes" aria-label="Catatan internal">
              {notes.map((n) => (
                <li key={n.id} className="note">
                  <span className="cell-av" aria-hidden="true">{initials(n.author.label)}</span>
                  <div>
                    <p className="note-by"><b>{n.author.label}</b> <span className="tiny muted">{instantLabel(n.createdAt, tz)}</span></p>
                    <p className="prewrap">{n.body}</p>
                  </div>
                </li>
              ))}
            </ol>
          )}
      {loadError && notes ? <p className="field-error" role="alert">{message(loadError)}</p> : null}
      {next && <Button onClick={() => page(next)}>Muat catatan lebih lama</Button>}
      {detail.can.addNote ? (
        <form className="formgrid composer" aria-label="Tambah catatan internal" onSubmit={(e) => { e.preventDefault(); void add(); }}>
          {error && <div className="full state-error" role="alert"><h3>{error.title}</h3><p>{error.body}</p></div>}
          <div className="field full">
            <label htmlFor={`${id}-note`}>Catatan penanganan *</label>
            <textarea id={`${id}-note`} className="field-control" value={text} maxLength={LIMITS.note} disabled={busy}
              aria-invalid={fieldErr ? true : undefined} aria-describedby={fieldErr ? `${id}-note-err` : undefined}
              onChange={(e) => { setText(e.target.value); setFieldErr(null); setNotice(null); }} />
            {fieldErr && <span id={`${id}-note-err`} className="field-error">{fieldErr}</span>}
          </div>
          <div className="full"><WordingAssistant key={`${workspace.id}:${detail.ticket.id}`} api={api} workspaceId={workspace.id} text={text}
            mode="reply" disabled={busy} onApply={setText} /></div>
          <div className="full row between">
            <span className="tiny muted">Maks. {num(LIMITS.note)} karakter · catatan tidak dapat diubah setelah disimpan · jangan menulis identitas atau data rahasia nasabah</span>
            <Button variant="primary" type="submit" disabled={busy} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan catatan'}</Button>
          </div>
        </form>
      ) : (
        <p className="tiny muted">{detail.can.readOnly ? 'Tiket ditutup: catatan baru tidak dapat ditambahkan.' : 'Peran Anda tidak dapat menambah catatan.'}</p>
      )}
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
    <div>
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
    </div>
  );
}

const KIND_LABEL: Record<AssignmentEvent['kind'], string> = {
  initial: 'PIC awal saat tiket dibuat',
  take_for_self: 'Diambil sendiri',
  reassign: 'Dialihkan',
};

/** Riwayat siklus: creation, then every resolve/reopen recorded by the server (oldest first). */
function CycleTimeline({ workspace, detail }: { workspace: Workspace; detail: TicketDetail }) {
  const tz = workspace.timezone;
  const p = detail.properties;
  const start = [p.intakeChannel?.label, p.category?.label].filter(Boolean).join(' · ');
  return (
    <ol className="tl" aria-label="Riwayat siklus">
      <li>
        <b>Tiket dibuat · siklus ke-1</b>
        <span className="tiny muted">{instantLabel(p.receivedAt, tz)}{start ? ` · ${start}` : ''}</span>
      </li>
      {detail.resolutions.map((r) => (
        <li key={r.id} className={r.action === 'resolve' ? 'ok' : 'warn'}>
          <b>{r.action === 'resolve' ? 'Diselesaikan' : 'Dibuka ulang'} · siklus ke-{r.cycleNo}</b>
          <span className="tiny muted">{instantLabel(r.at, tz)}{r.actor ? ` · oleh ${r.actor.label}` : ''}</span>
          {r.summary && <p className="tiny prewrap">{r.summary}</p>}
        </li>
      ))}
      {detail.resolutions.length === 0 && <li className="empty-tl"><span className="tiny muted">Belum pernah diselesaikan.</span></li>}
    </ol>
  );
}

/** Penanggung jawab: current PIC, the existing assign dialog, and the PIC history from the API. */
function Owner({ api, workspace, detail, canAssign, onAssign }: {
  api: Api;
  workspace: Workspace;
  detail: TicketDetail;
  canAssign: boolean;
  onAssign: () => void;
}) {
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
      {t.owner ? (
        <p className="cell-pic owner-now">
          <span className="cell-av" aria-hidden="true">{initials(t.owner.label)}</span>
          <span><b>{t.owner.label}</b><span className="tiny muted"> · PIC saat ini</span></span>
        </p>
      ) : <p className="muted tiny">Belum ditugaskan. Gunakan Penugasan untuk menetapkan PIC.</p>}
      {canAssign && <div className="row"><Button className="btn-small" onClick={onAssign}>{t.owner ? 'Ubah PIC' : 'Tetapkan PIC'}</Button></div>}
      <h4 className="eyebrow-h">Riwayat PIC</h4>
      {assign === null ? <p className="muted tiny" role="status">Memuat riwayat…</p>
        : assign === 'error' ? <p className="field-error" role="alert">Riwayat PIC belum dapat dimuat.</p>
          : assign.length === 0 ? <p className="muted tiny">Belum ada perubahan PIC tercatat.</p> : (
            <ol className="tl" aria-label="Riwayat PIC">
              {assign.map((h) => (
                <li key={h.id}>
                  <b>{KIND_LABEL[h.kind]}</b>
                  <span className="tiny muted">{instantLabel(h.at, tz)}{h.actor ? ` · oleh ${h.actor.label}` : ''}</span>
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
