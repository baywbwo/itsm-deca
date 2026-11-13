import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { ApiError, SESSION_RELOAD_TITLE, isSessionReloadNeeded, rateLimitText, sessionReloadText, isAbort, type Api } from '../api/client';
import type {
  Catalogs,
  CatalogItem,
  DirectoryEntry,
  IntakeDirectory,
  IntegrationSync,
  Member,
  TicketCreateRequest,
  TicketCreateResult,
  Workspace,
  WorkspacePermissions,
} from '../api/types';
import {
  drafts,
  emptyForm,
  formField,
  isDirty,
  newKey,
  toRequest,
  validateIntake,
  wallTime,
  type IntakeDraft,
  type IntakeFieldError,
  type IntakeForm,
} from '../app/drafts';
import { instantLabel, tzLabel } from '../app/format';
import { toHash } from '../app/routes';
import { Badge, type BadgeTone } from '../components/Badge';
import { Button } from '../components/Button';
import { WordingAssistant } from '../components/WordingAssistant';
import { Dialog } from '../components/Dialog';
import { statusTone } from '../components/Badge';

type Directory = { catalogs: Catalogs; directory: IntakeDirectory; members: Member[] };
type Load = { status: 'loading' } | { status: 'error'; error: unknown } | { status: 'ready'; data: Directory };
type Banner = { tone: 'error' | 'unknown'; title: string; body: string; newKey?: boolean };

const SYNC_TONE: Record<IntegrationSync['state'], BadgeTone> = {
  pending: 'waiting', success: 'ok', failed: 'error', unknown: 'waiting',
};
const SYNC_LABEL: Record<IntegrationSync['state'], string> = {
  pending: 'Menunggu', success: 'Berhasil', failed: 'Gagal', unknown: 'Tidak pasti',
};
const OPERATION_LABEL: Record<IntegrationSync['operation'], string> = {
  jira_create_issue: 'Jira · buat issue',
  jira_link_issue: 'Jira · tautkan issue',
  lark_ticket_created: 'Lark · notifikasi tiket dibuat',
  jira_transition_issue: 'Jira · ubah status issue',
};
const TARGET_STATUS_LABEL: Record<string, string> = {
  new: 'Baru', open: 'Dibuka', pending_customer: 'Menunggu nasabah', pending_engineering: 'Menunggu engineering',
  resolved: 'Selesai', closed: 'Ditutup',
};
const OUTCOME_LABEL = { success: 'berhasil', failed: 'gagal', unknown: 'tidak pasti', transient: 'gangguan sementara (dicoba ulang otomatis)' } as const;
/** Mirrors the server's bounded automatic retry budget (api/src/outbox.rs MAX_AUTO_ATTEMPTS). */
const AUTO_ATTEMPT_LIMIT = 5;

/**
 * O05 / D07 intake. Company is the active workspace (read-only, never sent). The draft lives in
 * memory per user + workspace and owns one idempotency key: a resend after a timeout uses the
 * same key and payload, so the server replays instead of creating a second ticket. Only a
 * server-confirmed create shows the receipt.
 */
export function IntakeDialog({ api, workspace, userId, permissions, onClose, onCreated, asPage = false }: {
  api: Api;
  workspace: Workspace;
  userId: string;
  permissions: WorkspacePermissions;
  onClose: () => void;
  onCreated: (r: TicketCreateResult) => void;
  /** CP-UI3: render as the "Tiket baru" page card (06) instead of a modal; same validation and submit. */
  asPage?: boolean;
}) {
  const tz = workspace.timezone;
  const id = useId();
  const [load, setLoad] = useState<Load>({ status: 'loading' });
  const [attempt, setAttempt] = useState(0);
  const restored = useRef(drafts.dirty(userId, workspace.id));
  const [draft, setDraftState] = useState<IntakeDraft>(() => drafts.get(userId, workspace.id) ?? fresh(tz));
  const [errors, setErrors] = useState<IntakeFieldError[]>([]);
  const [assist, setAssist] = useState(false);
  const [found, setFound] = useState<DirectoryEntry[]>([]);
  const [banner, setBanner] = useState<Banner | null>(null);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [focusNonce, setFocusNonce] = useState(0);
  const [receipt, setReceipt] = useState<TicketCreateResult | null>(null);
  const summaryRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const ctl = new AbortController();
    setLoad({ status: 'loading' });
    Promise.all([
      api.getCatalogs(workspace.id, ctl.signal),
      api.getIntakeDirectory(workspace.id, ctl.signal),
      api.listMembers(workspace.id, ctl.signal),
    ]).then(([catalogs, directory, members]) => setLoad({ status: 'ready', data: { catalogs, directory, members } }),
      (error: unknown) => { if (!isAbort(error)) setLoad({ status: 'error', error }); });
    return () => ctl.abort();
  }, [api, workspace.id, attempt]);

  // Keep the store in step: a dirty draft survives closing the dialog or switching workspace.
  const setDraft = (d: IntakeDraft) => {
    setDraftState(d);
    if (isDirty(d)) drafts.put(userId, workspace.id, d);
    else drafts.discard(userId, workspace.id);
  };
  const f = draft.form;
  const locked = busy || !!draft.inFlight;
  const set = <K extends keyof IntakeForm>(k: K, v: IntakeForm[K]) => {
    setDraft({ ...draft, form: { ...draft.form, [k]: v } });
    if (errors.some((e) => e.field === k)) setErrors(errors.filter((e) => e.field !== k));
  };
  const errorOf = (k: keyof IntakeForm) => errors.find((e) => e.field === k)?.message;
  const fid = (k: keyof IntakeForm) => `${id}-${k}`;

  useEffect(() => {
    if (!focusNonce) return;
    const first = errors[0];
    const el = first ? document.getElementById(fid(first.field)) : null;
    (el ?? summaryRef.current)?.focus();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusNonce]);

  async function send(body: TicketCreateRequest, key: string, base: IntakeDraft) {
    if (busyRef.current) return; // double click / Enter repeat: one request per key at a time
    busyRef.current = true;
    setBusy(true);
    setBanner(null);
    try {
      const res = await api.createTicket(workspace.id, key, body);
      drafts.discard(userId, workspace.id);
      setReceipt(res);
      onCreated(res);
    } catch (e) {
      const outcome = classify(e);
      if (outcome.kind === 'unknown') {
        // The ticket may exist: lock the payload and resend it with the same key only.
        setDraft({ ...base, inFlight: body });
        setBanner({ tone: 'unknown', title: 'Hasil pengiriman belum diketahui', body: outcome.message });
      } else {
        setDraft({ ...base, inFlight: null });
        const field = formField(outcome.field);
        if (field) {
          setErrors([{ field, message: outcome.message }]);
          setFocusNonce((n) => n + 1);
        } else {
          setBanner({ tone: 'error', title: outcome.title, body: outcome.message, ...(outcome.newKey ? { newKey: true } : {}) });
          setFocusNonce((n) => n + 1);
        }
      }
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  function submit() {
    if (locked) return;
    const errs = validateIntake(f);
    setErrors(errs);
    if (errs.length) {
      setFocusNonce((n) => n + 1);
      return;
    }
    void send(toRequest(f, tz), draft.key, draft);
  }

  function discard() {
    drafts.discard(userId, workspace.id);
    restored.current = false;
    setDraftState(fresh(tz));
    setErrors([]);
    setBanner(null);
  }

  if (receipt) {
    return <Receipt api={api} workspace={workspace} result={receipt} onClose={onClose}
      onAnother={() => { setReceipt(null); restored.current = false; setDraftState(fresh(tz)); }} />;
  }

  const data = load.status === 'ready' ? load.data : null;
  const active = (l: CatalogItem[] | undefined) => (l ?? []).filter((c) => c.active);
  const canAny = permissions.permissions.includes('ticket.assign_any');
  const canSelf = permissions.permissions.includes('ticket.assign_self');
  const activeMembers = (data?.members ?? []).filter((m) => m.active);
  const ownerChoices = canAny ? activeMembers : canSelf ? activeMembers.filter((m) => m.userId === userId) : [];
  const jira = data?.directory.jira ?? null;
  const lark = data?.directory.lark ?? null;
  const jiraUsable = !!jira?.usable;
  const larkUsable = !!lark?.usable;
  const live = data?.directory.integrationsMode === 'live';
  const assigneeChoices = mergeEntries(jira?.options ?? [], found);

  const select = (k: keyof IntakeForm, label: string, items: { id: string; label: string }[], opts: {
    required?: boolean; empty?: string; hint?: ReactNode; disabled?: boolean; full?: boolean;
  } = {}) => (
    <div className={opts.full ? 'field full' : 'field'}>
      <label htmlFor={fid(k)}>{label}{opts.required && <Req />}</label>
      <select id={fid(k)} className="field-control" value={f[k]} disabled={locked || opts.disabled}
        aria-required={opts.required || undefined} aria-invalid={errorOf(k) ? true : undefined}
        aria-describedby={[opts.hint ? `${fid(k)}-hint` : '', errorOf(k) ? `${fid(k)}-error` : ''].filter(Boolean).join(' ') || undefined}
        onChange={(e) => set(k, e.target.value as never)}>
        <option value="">{opts.empty ?? 'Tidak diisi'}</option>
        {items.map((o) => <option key={o.id} value={o.id}>{o.label}</option>)}
      </select>
      {opts.hint && <span id={`${fid(k)}-hint`} className="field-hint">{opts.hint}</span>}
      {errorOf(k) && <span id={`${fid(k)}-error`} className="field-error">{errorOf(k)}</span>}
    </div>
  );
  const catalog = (l: CatalogItem[] | undefined) => active(l).map((c) => ({ id: c.id, label: c.label }));

  const text = (key: 'subject' | 'merchantName' | 'reporterEmail' | 'reporterPhone' | 'reporterName', label: string, placeholder: string, opts: {
    type?: 'text' | 'email' | 'tel'; required?: boolean; full?: boolean; max?: number; autoComplete?: string;
  } = {}) => (
    <div className={opts.full ? 'field full' : 'field'}>
      <label htmlFor={fid(key)}>{label}{opts.required && <Req />}</label>
      <input id={fid(key)} type={opts.type ?? 'text'} className="field-control" value={f[key]} maxLength={opts.max ?? 254}
        placeholder={placeholder} autoComplete={opts.autoComplete ?? 'off'} aria-required={opts.required || undefined}
        aria-invalid={errorOf(key) ? true : undefined}
        aria-describedby={errorOf(key) ? `${fid(key)}-error` : undefined}
        onChange={(e) => set(key, e.target.value)} />
      {errorOf(key) && <span id={`${fid(key)}-error`} className="field-error">{errorOf(key)}</span>}
    </div>
  );
  const operationError = errors.some((e) => OPERATION_FIELDS.includes(e.field));
  const providerError = errors.some((e) => PROVIDER_FIELDS.includes(e.field));

  const discardButton = (restored.current || isDirty(draft)) && !busy
    ? <Button variant="ghost" onClick={discard}>Buang draf</Button> : null;
  const cancelButton = <Button onClick={onClose} disabled={busy}>{isDirty(draft) ? 'Tutup (draf disimpan)' : 'Batal'}</Button>;
  const primaryButton = draft.inFlight ? (
    <Button variant="primary" disabled={busy} aria-busy={busy}
      onClick={() => void send(draft.inFlight!, draft.key, draft)}>
      {busy ? 'Mengirim ulang…' : 'Kirim ulang (aman, tanpa duplikat)'}
    </Button>
  ) : (
    <Button variant="primary" type="submit" form={`${id}-form`} disabled={busy || !data} aria-busy={busy}>
      {busy ? 'Menyimpan…' : asPage ? 'Simpan tiket' : 'Buat tiket'}
    </Button>
  );

  const body = (
    <>
      <p className="muted tiny">Perusahaan pelapor: <strong>{workspace.code} · {workspace.name}</strong> · mengikuti
        perusahaan Anda (tidak dapat diubah di sini)</p>
      {restored.current && (
        <p className="notice tiny" role="note">Draf belum terkirim dipulihkan. Draf hanya disimpan di memori tab ini
          untuk akun dan perusahaan ini; hilang saat halaman dimuat ulang.</p>
      )}
      <div ref={summaryRef} tabIndex={-1} aria-live="assertive">
        {errors.length > 1 && (
          <div className="state-error spaced-bottom" role="alert">
            <h3>{errors.length} isian perlu diperbaiki</h3>
            <ul className="plain">
              {errors.map((e) => (
                <li key={e.field}>
                  <a href={`#${fid(e.field)}`} onClick={(ev) => { ev.preventDefault(); document.getElementById(fid(e.field))?.focus(); }}>
                    {e.message}
                  </a>
                </li>
              ))}
            </ul>
          </div>
        )}
        {banner && (
          <div className={banner.tone === 'unknown' ? 'state-warn spaced-bottom' : 'state-error spaced-bottom'} role="alert">
            <h3>{banner.title}</h3>
            <p>{banner.body}</p>
            {banner.newKey && (
              <Button onClick={() => { setDraft({ ...draft, key: newKey() }); setBanner(null); }}>
                Kirim sebagai tiket baru (kunci baru)
              </Button>
            )}
          </div>
        )}
        {draft.inFlight && !banner && (
          <div className="state-warn spaced-bottom" role="status">
            <h3>Pengiriman sebelumnya belum pasti</h3>
            <p>Isian dikunci. Kirim ulang memakai kunci dan isian yang sama sehingga server tidak membuat tiket kedua.</p>
          </div>
        )}
      </div>
      {load.status === 'loading' && <div role="status"><p className="muted">Memuat pilihan…</p><div className="skeleton" aria-hidden="true" /></div>}
      {load.status === 'error' && (
        <div className="state-error" role="alert">
          <h3>Pilihan form belum dapat dimuat</h3>
          <p>{load.error instanceof ApiError ? load.error.message : 'Terjadi kesalahan tak terduga.'} Isian Anda tetap.</p>
          <Button onClick={() => setAttempt((n) => n + 1)}>Coba lagi</Button>
        </div>
      )}
      {data && (
        <form id={`${id}-form`} className="formgrid" noValidate onSubmit={(e) => { e.preventDefault(); submit(); }}>
          <fieldset className="plain-fieldset full formgrid" disabled={locked}>
            <legend className="sr-only">Data pengaduan</legend>
            {text('subject', 'Subjek', 'Ringkasan masalah, maks. 300 karakter', { required: true, full: true, max: 300 })}
            {text('merchantName', 'Nama merchant / usaha', 'Contoh: Toko Melati')}
            {text('reporterEmail', 'Email akun terdaftar', 'nama@perusahaan.com', { type: 'email', autoComplete: 'email' })}
            {text('reporterName', 'Nama PIC / pelapor', 'Nama orang yang menghubungi')}
            {text('reporterPhone', 'WhatsApp (opsional)', '08xxxxxxxxxx', { type: 'tel', autoComplete: 'tel', max: 40 })}
            {select('categoryId', 'Kategori', catalog(data.catalogs.categories), { empty: 'Pilih kategori' })}
            {select('intakeChannelId', 'Kanal diterima', catalog(data.catalogs.intakeChannels), { required: true, empty: 'Pilih kanal' })}
            {select('priorityId', 'Prioritas', catalog(data.catalogs.priorities))}
            {select('productId', 'Produk / layanan', catalog(data.catalogs.products), { empty: 'Belum ditentukan' })}
            <div className="field full">
              <label htmlFor={fid('chronology')}>Kronologi<Req /></label>
              <textarea id={fid('chronology')} className="field-control" value={f.chronology} maxLength={20000} aria-required
                placeholder="Uraikan kejadian: waktu, gejala, langkah yang sudah dicoba…"
                aria-invalid={errorOf('chronology') ? true : undefined}
                aria-describedby={`${fid('chronology')}-hint${errorOf('chronology') ? ` ${fid('chronology')}-error` : ''}`}
                onChange={(e) => set('chronology', e.target.value)} />
              <div className="row between intake-chrono-foot">
                <span id={`${fid('chronology')}-hint`} className="field-hint">Maks. 20.000 karakter. Dapat memuat data pribadi: tidak disalin ke audit, log, Jira atau Lark.</span>
                <Button className="btn-small" aria-expanded={assist} aria-controls={`${id}-assist`} onClick={() => setAssist((v) => !v)}>
                  <span aria-hidden="true">✦</span> Bantu tulis
                </Button>
              </div>
              {errorOf('chronology') && <span id={`${fid('chronology')}-error`} className="field-error">{errorOf('chronology')}</span>}
            </div>
            {/* CP-UI3b: the GPT assistant opens only on "Bantu tulis" (prototype 06), as on the ticket detail. */}
            <div className="full" id={`${id}-assist`} hidden={!assist}>
              {assist && <WordingAssistant key={workspace.id} api={api} workspaceId={workspace.id} text={f.chronology}
                mode="chronology" disabled={locked} onApply={text => set('chronology', text)} />}
            </div>
            <details className="intake-operations full" open={operationError || undefined}>
              <summary>Data penerimaan & penanganan <span>Waktu diterima, PIC, unit, wilayah, dan direktori merchant</span></summary>
              <div className="formgrid spaced">
                <div className="field">
                  <label htmlFor={fid('receivedLocal')}>Waktu diterima<Req /></label>
                  <input id={fid('receivedLocal')} type="datetime-local" className="field-control" value={f.receivedLocal}
                    max={wallTime(Date.now() + 5 * 60000, tz)} aria-required
                    aria-invalid={errorOf('receivedLocal') ? true : undefined}
                    aria-describedby={`${fid('receivedLocal')}-hint${errorOf('receivedLocal') ? ` ${fid('receivedLocal')}-error` : ''}`}
                    onChange={(e) => set('receivedLocal', e.target.value)} />
                  <span id={`${fid('receivedLocal')}-hint`} className="field-hint">Waktu {tzLabel(tz)} ({tz}).</span>
                  {errorOf('receivedLocal') && <span id={`${fid('receivedLocal')}-error`} className="field-error">{errorOf('receivedLocal')}</span>}
                </div>
                {select('ownerUserId', 'PIC CS', ownerChoices.map((m) => ({ id: m.userId, label: m.userId === userId ? `${m.displayName} (saya)` : m.displayName })), {
                  empty: 'Belum ditugaskan',
                  disabled: ownerChoices.length === 0,
                  hint: canAny ? 'Anggota aktif perusahaan ini.' : canSelf ? 'Peran Anda hanya dapat menugaskan diri sendiri.' : 'Peran Anda tidak dapat menetapkan PIC.',
                })}
                {select('receivingUnitId', 'Unit penerima', catalog(data.catalogs.receivingUnits))}
                {select('regionId', 'Wilayah', catalog(data.catalogs.regions))}
                {select('merchantId', 'Tautkan merchant dari direktori', (data.directory.merchants).map((m) => ({ id: m.id, label: `${m.displayName}${m.reference ? ` · ${m.reference}` : ''} · DUMMY` })), {
                  full: true, hint: 'Data merchant dummy berlabel, bukan hasil CIF. Nomor identitas tidak diminta.',
                })}
              </div>
            </details>
          </fieldset>
          <details className="intake-operations full" open={f.jiraMode !== 'none' || providerError || undefined}>
            <summary>Assignment & notifikasi <span>Pekerjaan Jira dan channel Lark (opsional)</span></summary>
            <fieldset className="plain-fieldset formgrid spaced" disabled={locked}>
              <legend className="sr-only">Assignment & notifikasi</legend>
              <p className="tiny muted full">
                {!live && <><Badge tone="neutral">Mode simulasi</Badge>{' '}</>}
                PIC CS menangani tiket. Assignee Jira mengerjakan pekerjaan terkait.
                {live
                  ? ' Issue dan notifikasi dikirim ke Jira/Lark setelah tiket tersimpan; tiket tetap tersimpan bila pengiriman gagal.'
                  : ' Server dalam mode simulasi: tidak ada issue atau pesan yang dikirim ke Jira/Lark. Tiket tetap tersimpan bila Jira/Lark gagal.'}</p>
              <div className="field">
                <label htmlFor={fid('jiraMode')}>Pekerjaan Jira</label>
                <select id={fid('jiraMode')} className="field-control" value={f.jiraMode} disabled={!jiraUsable}
                  aria-describedby={`${fid('jiraMode')}-hint`} aria-invalid={errorOf('jiraMode') ? true : undefined}
                  onChange={(e) => set('jiraMode', e.target.value as IntakeForm['jiraMode'])}>
                  <option value="none">Tidak ditautkan</option>
                  {jiraUsable && <option value="create">Buat issue</option>}
                  {jiraUsable && <option value="link">Tautkan issue</option>}
                </select>
                <span id={`${fid('jiraMode')}-hint`} className="field-hint">{providerHint(jira, 'Jira')}</span>
                {errorOf('jiraMode') && <span className="field-error">{errorOf('jiraMode')}</span>}
              </div>
              {f.jiraMode !== 'none' && (
                <>
                  {f.jiraMode === 'link' && (
                    <div className="field">
                      <label htmlFor={fid('jiraIssueKey')}>Kunci issue<Req /></label>
                      <input id={fid('jiraIssueKey')} className="field-control" value={f.jiraIssueKey} maxLength={64}
                        placeholder={live ? 'mis. OPS-101' : `mis. SIM-${workspace.code}-101`} aria-required
                        aria-invalid={errorOf('jiraIssueKey') ? true : undefined}
                        aria-describedby={`${fid('jiraIssueKey')}-hint${errorOf('jiraIssueKey') ? ` ${fid('jiraIssueKey')}-error` : ''}`}
                        onChange={(e) => set('jiraIssueKey', e.target.value)} />
                      <span id={`${fid('jiraIssueKey')}-hint`} className="field-hint">{live ? 'Issue di proyek Jira koneksi ini; diperiksa ke Jira saat dikirim.' : 'Issue yang dikenal direktori Jira perusahaan ini.'}</span>
                      {errorOf('jiraIssueKey') && <span id={`${fid('jiraIssueKey')}-error`} className="field-error">{errorOf('jiraIssueKey')}</span>}
                    </div>
                  )}
                  <div className="field">
                    <label htmlFor={fid('jiraTitle')}>{f.jiraMode === 'create' ? <>Judul issue<Req /></> : 'Judul pekerjaan terkait'}</label>
                    <input id={fid('jiraTitle')} className="field-control" value={f.jiraTitle} maxLength={300}
                      aria-required={f.jiraMode === 'create' || undefined} aria-invalid={errorOf('jiraTitle') ? true : undefined}
                      aria-describedby={`${fid('jiraTitle')}-hint${errorOf('jiraTitle') ? ` ${fid('jiraTitle')}-error` : ''}`}
                      placeholder="Tanpa identitas pelanggan" onChange={(e) => set('jiraTitle', e.target.value)} />
                    <span id={`${fid('jiraTitle')}-hint`} className="field-hint">
                      Hanya disimpan di ITSM. Jira menerima judul otomatis "[ITSM] nomor tiket — kategori"; jangan tulis data pribadi nasabah.
                    </span>
                    {errorOf('jiraTitle') && <span id={`${fid('jiraTitle')}-error`} className="field-error">{errorOf('jiraTitle')}</span>}
                  </div>
                  {live && (
                    <JiraUserSearch api={api} workspaceId={workspace.id} inputId={`${id}-jira-user-search`}
                      onFound={(items) => setFound((prev) => mergeEntries(prev, items))} />
                  )}
                  {select('jiraAssigneeId', 'Assignee Jira', assigneeChoices.map((o) => ({ id: o.id, label: o.displayName })), {
                    empty: 'Belum ditugaskan',
                    hint: live ? 'Pengguna Jira dari pencarian di samping, atau yang pernah dipilih sebelumnya.' : undefined,
                  })}
                </>
              )}
              {select('larkDestinationId', 'Channel Lark', larkUsable ? lark!.options.map((o) => ({ id: o.id, label: o.displayName })) : [], {
                empty: 'Tanpa notifikasi', disabled: !larkUsable, hint: larkUsable ? 'Grup menerima subjek tiket, produk, PIC, prioritas, dan tautan Jira. Hindari data pribadi pada subjek.' : providerHint(lark, 'Lark'),
              })}
            </fieldset>
          </details>
          <button type="submit" hidden tabIndex={-1} aria-hidden="true" />
        </form>
      )}
    </>
  );

  if (asPage) {
    return (
      <section className="pcard intake-page" aria-labelledby={`${id}-title`}>
        <div className="hd"><h2 id={`${id}-title`}>Data tiket</h2><span className="sub"><span className="req" aria-hidden="true">*</span> wajib diisi</span></div>
        <div className="bd">
          {body}
          <div className="intake-foot">
            {primaryButton}
            {cancelButton}
            {discardButton}
            <span className="foot-note">Nomor tiket dibuat otomatis per perusahaan</span>
          </div>
        </div>
      </section>
    );
  }

  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title="Buat tiket baru" initialFocus={`#${CSS.escape(fid('subject'))}`}
      footer={<>{discardButton}{cancelButton}{primaryButton}</>}>
      {body}
    </Dialog>
  );
}

const OPERATION_FIELDS: readonly string[] = ['receivedLocal', 'receivingUnitId', 'regionId', 'ownerUserId', 'merchantId'];
const PROVIDER_FIELDS: readonly string[] = ['jiraMode', 'jiraIssueKey', 'jiraTitle', 'jiraAssigneeId', 'larkDestinationId'];

/** Required marker: red, hidden from screen readers (the control already carries aria-required). */
const Req = () => <span className="req" aria-hidden="true">*</span>;

function fresh(tz: string): IntakeDraft {
  const form = emptyForm(wallTime(Date.now(), tz));
  return { form, initial: form, key: newKey(), inFlight: null };
}

function providerHint(p: IntakeDirectory['jira'], name: string): string {
  if (!p) return `${name} belum dikonfigurasi untuk perusahaan ini.`;
  if (p.state === 'disabled') return `${p.displayName} dinonaktifkan.`;
  if (!p.usable) return `${p.displayName} belum aktif; admin perlu mengatur, menguji, dan mengaktifkannya di Integrasi.`;
  if (p.mode === 'live') return `${p.displayName} · tersambung.`;
  return `${p.displayName} · simulasi, hasil terkonfigurasi: ${OUTCOME_LABEL[p.simulatedOutcome]}.`;
}

function mergeEntries(a: DirectoryEntry[], b: DirectoryEntry[]): DirectoryEntry[] {
  const seen = new Set(a.map((x) => x.id));
  return [...a, ...b.filter((x) => !seen.has(x.id))];
}

/** Live mode: finds assignees in the connection's Jira (min. 2 characters, debounced); results join the Assignee list. */
function JiraUserSearch({ api, workspaceId, inputId, onFound }: {
  api: Api; workspaceId: string; inputId: string; onFound: (items: DirectoryEntry[]) => void;
}) {
  const [query, setQuery] = useState('');
  const [status, setStatus] = useState('');
  const found = useRef(onFound);
  found.current = onFound;
  useEffect(() => {
    const q = query.trim();
    if (q.length < 2) { setStatus(''); return undefined; }
    const ctl = new AbortController();
    const t = window.setTimeout(() => {
      setStatus('Mencari di Jira…');
      api.searchJiraUsers(workspaceId, q, ctl.signal).then((r) => {
        found.current(r.items);
        setStatus(r.items.length ? `${r.items.length} pengguna Jira ditemukan; pilih di daftar Assignee Jira.` : 'Tidak ada pengguna Jira yang cocok.');
      }, (e: unknown) => {
        if (!isAbort(e)) setStatus(e instanceof ApiError ? `Pencarian gagal: ${e.message}` : 'Pencarian gagal.');
      });
    }, 350);
    return () => { window.clearTimeout(t); ctl.abort(); };
  }, [api, workspaceId, query]);
  return (
    <div className="field">
      <label htmlFor={inputId}>Cari pengguna Jira</label>
      <input id={inputId} type="search" className="field-control" value={query} maxLength={100} autoComplete="off"
        placeholder="Nama atau email, min. 2 huruf" aria-describedby={`${inputId}-status`}
        onKeyDown={(e) => { if (e.key === 'Enter') e.preventDefault(); }}
        onChange={(e) => setQuery(e.target.value)} />
      <span id={`${inputId}-status`} className="field-hint" role="status" aria-live="polite">{status}</span>
    </div>
  );
}

type Outcome =
  | { kind: 'unknown'; message: string }
  | { kind: 'definite'; title: string; message: string; field?: string; newKey?: boolean };

/** Which failures are definite (nothing stored) and which leave the outcome unknown. */
function classify(e: unknown): Outcome {
  if (!(e instanceof ApiError)) return { kind: 'unknown', message: 'Terjadi kesalahan tak terduga; tiket mungkin sudah tersimpan. Kirim ulang aman.' };
  const rid = e.requestId ? ` (ID permintaan ${e.requestId})` : '';
  if (e.code === 'timeout' || e.code === 'network' || e.status >= 500 || e.status === 0) {
    return { kind: 'unknown', message: `Server belum memberi jawaban pasti; tiket mungkin sudah tersimpan. Kirim ulang memakai kunci yang sama tidak membuat duplikat.${rid}` };
  }
  if (isSessionReloadNeeded(e)) return { kind: 'definite', title: SESSION_RELOAD_TITLE, message: sessionReloadText(e.requestId) };
  switch (e.status) {
    case 429:
      return { kind: 'definite', title: 'Terlalu banyak permintaan', message: `${rateLimitText(e)} Tiket belum dibuat; isian Anda tetap di sini dan kirim ulang memakai kunci yang sama.${rid}` };
    case 422:
      if (e.field) return { kind: 'definite', title: 'Isian ditolak', message: fieldMessage(e.field), field: e.field };
      return { kind: 'definite', title: 'Isian ditolak', message: `Periksa isian lalu coba lagi. Tidak ada tiket yang dibuat.${rid}` };
    case 409:
      return { kind: 'definite', title: 'Kunci pengiriman sudah dipakai', newKey: true,
        message: `Kunci draf ini sudah dipakai untuk tiket dengan isian berbeda. Periksa daftar tiket agar tidak ganda; isian Anda tetap di sini.${rid}` };
    case 403:
      return { kind: 'definite', title: 'Tidak diizinkan', message: `Peran Anda tidak dapat membuat tiket atau menetapkan PIC ini. Tidak ada tiket yang dibuat.${rid}`, field: e.field ?? 'ownerUserId' };
    case 401:
      return { kind: 'definite', title: 'Sesi berakhir', message: 'Muat ulang halaman untuk masuk lagi. Draf di memori akan hilang saat dimuat ulang.' };
    case 404:
      return { kind: 'definite', title: 'Perusahaan tidak ditemukan', message: `Perusahaan ini tidak ada atau tidak dapat Anda akses.${rid}` };
    default:
      return { kind: 'definite', title: 'Isian tidak valid', message: `${e.message} Tidak ada tiket yang dibuat.${rid}` };
  }
}

function fieldMessage(field: string): string {
  switch (field) {
    case 'receivedAt': return 'Waktu diterima tidak boleh lebih dari 5 menit ke depan atau sebelum tahun 2000.';
    case 'ownerUserId': return 'PIC harus anggota aktif perusahaan ini.';
    case 'merchantId': return 'Merchant tidak dikenal di perusahaan ini.';
    case 'jira.issueKey': return 'Kunci issue tidak valid atau tidak dikenal di koneksi Jira perusahaan ini (format PROYEK-123).';
    case 'jira.assigneeId': return 'Assignee Jira tidak terdaftar di koneksi perusahaan ini.';
    case 'jira.mode': case 'jira': return 'Koneksi Jira perusahaan ini tidak tersedia.';
    case 'larkDestinationId': return 'Channel Lark tidak tersedia di perusahaan ini.';
    case 'subject': return 'Judul wajib diisi (maksimal 300 karakter).';
    case 'chronology': return 'Kronologi wajib diisi (maksimal 20.000 karakter).';
    case 'jira.title': return 'Judul issue Jira wajib diisi (maksimal 300 karakter).';
    default: return 'Pilihan ini tidak aktif atau bukan milik perusahaan ini. Pilih ulang.';
  }
}

/** Server-confirmed result only: number, link to the List, integrations kept separate. */
function Receipt({ api, workspace, result, onClose, onAnother }: {
  api: Api;
  workspace: Workspace;
  result: TicketCreateResult;
  onClose: () => void;
  onAnother: () => void;
}) {
  const t = result.ticket;
  const href = toHash({ workspaceId: workspace.id, page: 'tickets', query: `q=${encodeURIComponent(t.displayNumber)}` });
  return (
    <Dialog open onClose={onClose} title="Tiket tersimpan" initialFocus=".receipt-head"
      footer={(
        <>
          <Button onClick={onAnother}>Buat tiket lain</Button>
          <Button variant="primary" onClick={onClose}>Selesai</Button>
        </>
      )}>
      <div className="receipt-head" tabIndex={-1} role="status">
        <p className="eyebrow">{result.replayed ? 'Dikenali sebagai kiriman ulang' : 'Dikonfirmasi server'}</p>
        <h3>{t.displayNumber} · {t.subject}</h3>
        <p className="tiny muted">
          {result.replayed
            ? 'Server mengembalikan tiket yang sudah dibuat dengan kunci ini; tidak ada tiket kedua.'
            : 'Tiket tersimpan sekali di database dan tercatat di audit.'}
        </p>
      </div>
      <dl className="props">
        <div className="props-row"><dt>Status</dt><dd><Badge tone={statusTone(t.status)}>{t.status.label}</Badge></dd></div>
        <div className="props-row"><dt>PIC</dt><dd>{t.owner?.label ?? 'Belum ditugaskan'}</dd></div>
        <div className="props-row"><dt>Kanal</dt><dd>{t.channel?.label ?? '—'}</dd></div>
        <div className="props-row"><dt>Diterima</dt><dd>{instantLabel(t.receivedAt, workspace.timezone)}</dd></div>
      </dl>
      <p className="spaced"><a href={href} onClick={onClose}>Lihat {t.displayNumber} di daftar tiket</a></p>
      <IntegrationPanel api={api} workspaceId={workspace.id} ticketId={t.id} timezone={workspace.timezone} initial={result.integrations} />
      <p className="tiny muted spaced">Catatan tindak lanjut dan riwayat ada di detail tiket (buka dari daftar tiket).</p>
    </Dialog>
  );
}

/** Jira/Lark sync state per intent. Failed → permissioned retry; unknown → reconcile with evidence. */
export function IntegrationPanel({ api, workspaceId, ticketId, timezone, initial }: {
  api: Api;
  workspaceId: string;
  ticketId: string;
  timezone: string;
  initial: IntegrationSync[];
}) {
  const [items, setItems] = useState(initial);
  const [canRecover, setCanRecover] = useState(false);
  const [loading, setLoading] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [reconciling, setReconciling] = useState<string | null>(null);

  const refresh = async (signal?: AbortSignal) => {
    setLoading(true);
    try {
      const r = await api.getTicketIntegrations(workspaceId, ticketId, signal);
      setItems(r.items);
      setCanRecover(r.canRecover);
      setMsg(null);
    } catch (e) {
      if (!isAbort(e)) setMsg('Status integrasi belum dapat dimuat ulang. Tiket tetap tersimpan.');
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => {
    const ctl = new AbortController();
    void refresh(ctl.signal);
    return () => ctl.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspaceId, ticketId]);

  const replace = (it: IntegrationSync) => setItems((l) => l.map((x) => (x.id === it.id ? it : x)));

  async function retry(it: IntegrationSync) {
    setMsg(null);
    try {
      const r = await api.retryOutbox(workspaceId, it.id, { attempt: it.attempt });
      replace(r.item);
      setMsg(`Percobaan ulang ${OPERATION_LABEL[it.operation]}: ${SYNC_LABEL[r.item.state].toLowerCase()}${r.item.mode === 'simulation' ? ' (simulasi)' : ''}.`);
    } catch (e) {
      setMsg(recoveryError(e));
      if (e instanceof ApiError && (e.status === 409 || e.status === 422)) void refresh();
    }
  }

  if (items.length === 0) {
    return <section className="card"><h3>Integrasi</h3><p className="muted tiny">Tidak ada Jira/Lark yang dipilih untuk tiket ini.</p></section>;
  }
  return (
    <section className="card integration-panel" aria-labelledby={`${ticketId}-int`}>
      <div className="row between">
        <h3 id={`${ticketId}-int`}>Integrasi{items.every((it) => it.mode === 'simulation') && <> <Badge>Simulasi</Badge></>}</h3>
        <Button className="btn-small" onClick={() => void refresh()} disabled={loading} aria-busy={loading}>
          {loading ? 'Memuat…' : 'Muat ulang status'}
        </Button>
      </div>
      <ul className="plain sync-list">
        {items.map((it) => (
          <li key={it.id} className="sync-item">
            <div className="row between">
              <strong>{OPERATION_LABEL[it.operation]}</strong>
              <Badge tone={SYNC_TONE[it.state]}>{it.mode === 'simulation' ? `${SYNC_LABEL[it.state]} · simulasi` : SYNC_LABEL[it.state]}</Badge>
            </div>
            <p className="tiny muted">
              {[it.targetLabel, it.targetStatus && `ikut tiket: ${TARGET_STATUS_LABEL[it.targetStatus] ?? it.targetStatus}`, it.issueKey && `Issue ${it.issueKey}`, `percobaan ${it.attempt}`, it.reconciled && 'direkonsiliasi'].filter(Boolean).join(' · ')}
            </p>
            {it.issueUrl && <p className="tiny"><a href={it.issueUrl} target="_blank" rel="noopener noreferrer">Buka {it.issueKey ?? 'issue'} di Jira</a></p>}
            {it.safeError && <p className="tiny">{it.safeError}</p>}
            {it.state === 'pending' && <p className="tiny muted">{pendingNote(it, timezone)}</p>}
            {it.state === 'failed' && (canRecover
              ? <Button className="btn-small" onClick={() => void retry(it)}>Kirim ulang</Button>
              : <p className="tiny muted">Pemulihan oleh pengelola integrasi berizin.</p>)}
            {it.state === 'unknown' && (canRecover
              ? (reconciling === it.id
                ? <ReconcileForm api={api} workspaceId={workspaceId} item={it}
                    onDone={(n) => { replace(n); setReconciling(null); setMsg(`Rekonsiliasi tersimpan: ${SYNC_LABEL[n.state].toLowerCase()}.`); }}
                    onError={(m) => { setMsg(m); void refresh(); }}
                    onCancel={() => setReconciling(null)} />
                : <Button className="btn-small" onClick={() => setReconciling(it.id)}>Rekonsiliasi dengan bukti</Button>)
              : <p className="tiny muted">Hasil tidak pasti: wajib direkonsiliasi oleh pengelola integrasi berizin, bukan dikirim ulang.</p>)}
          </li>
        ))}
      </ul>
      <p className="sr-only" role="status" aria-live="polite">{msg ?? ''}</p>
      {msg && <p className="tiny">{msg}</p>}
    </section>
  );
}

function ReconcileForm({ api, workspaceId, item, onDone, onError, onCancel }: {
  api: Api;
  workspaceId: string;
  item: IntegrationSync;
  onDone: (i: IntegrationSync) => void;
  onError: (m: string) => void;
  onCancel: () => void;
}) {
  const id = useId();
  const [outcome, setOutcome] = useState<'success' | 'failed'>('failed');
  const [evidence, setEvidence] = useState('');
  const [issueKey, setIssueKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const needKey = outcome === 'success' && item.operation === 'jira_create_issue';

  async function submit() {
    if (busy) return;
    if (!evidence.trim()) return setErr('Bukti pemeriksaan wajib diisi.');
    if (needKey && !issueKey.trim()) return setErr('Kunci issue yang ditemukan wajib diisi.');
    setBusy(true);
    setErr(null);
    try {
      const r = await api.reconcileOutbox(workspaceId, item.id, {
        attempt: item.attempt, outcome, evidence: evidence.trim(), ...(needKey ? { issueKey: issueKey.trim() } : {}),
      });
      onDone(r.item);
    } catch (e) {
      setBusy(false);
      onError(recoveryError(e));
    }
  }

  return (
    <form className="formgrid reconcile" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
      <div className="field">
        <label htmlFor={`${id}-o`}>Hasil yang dipastikan</label>
        <select id={`${id}-o`} className="field-control" value={outcome} onChange={(e) => setOutcome(e.target.value as 'success' | 'failed')}>
          <option value="failed">Tidak terkirim (gagal)</option>
          <option value="success">Sudah terkirim (berhasil)</option>
        </select>
      </div>
      {needKey && (
        <div className="field">
          <label htmlFor={`${id}-k`}>Kunci issue yang ditemukan *</label>
          <input id={`${id}-k`} className="field-control" value={issueKey} maxLength={64} onChange={(e) => setIssueKey(e.target.value)} />
        </div>
      )}
      <div className="field full">
        <label htmlFor={`${id}-e`}>Bukti pemeriksaan *</label>
        <textarea id={`${id}-e`} className="field-control" value={evidence} maxLength={2000}
          aria-describedby={`${id}-eh`} onChange={(e) => setEvidence(e.target.value)} />
        <span id={`${id}-eh`} className="field-hint">{item.mode === 'live' ? 'Mis. hasil cek langsung di Jira/Lark.' : 'Mis. hasil cek di sisi penyedia (simulasi).'} Teks tidak disalin ke audit.</span>
      </div>
      {err && <p className="field-error full" role="alert">{err}</p>}
      <div className="row full">
        <Button onClick={onCancel} disabled={busy}>Batal</Button>
        <Button variant="primary" type="submit" disabled={busy} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan rekonsiliasi'}</Button>
      </div>
    </form>
  );
}

function recoveryError(e: unknown): string {
  if (e instanceof ApiError) {
    if (isSessionReloadNeeded(e)) return `${SESSION_RELOAD_TITLE}. ${sessionReloadText(e.requestId)}`;
    if (e.status === 429) return `${rateLimitText(e)} Aksi pemulihan belum dikirim.`;
    if (e.status === 409) return 'Status integrasi sudah berubah oleh pihak lain. Status terbaru dimuat ulang.';
    if (e.status === 422) return 'Aksi ini tidak berlaku untuk status integrasi sekarang.';
    if (e.status === 403) return 'Anda tidak memiliki izin pemulihan integrasi.';
    return `${e.message}`;
  }
  return 'Terjadi kesalahan tak terduga.';
}

function pendingNote(it: IntegrationSync, tz: string): string {
  if (it.inFlight) return `Sedang dikirim${it.mode === 'simulation' ? ' (simulasi)' : ''}. Muat ulang status untuk melihat hasil.`;
  if (it.nextAttemptAt) {
    return `Gangguan sementara; dicoba ulang otomatis sekitar ${instantLabel(it.nextAttemptAt, tz)} (percobaan otomatis ${it.autoAttempts} dari ${AUTO_ATTEMPT_LIMIT}).`;
  }
  return 'Belum selesai; antrean dikirim otomatis. Muat ulang status untuk melihat hasil.';
}
