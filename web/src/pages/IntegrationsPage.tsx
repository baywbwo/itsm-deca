import { useId, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type {
  IntegrationConnection, IntegrationIdName, IntegrationList, JiraOptions, ProviderCode, Workspace,
} from '../api/types';
import { instantLabel } from '../app/format';
import { credentialRetargeted } from '../app/integrations';
import { Badge, type BadgeTone } from '../components/Badge';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { SelectField, TextField } from '../components/Field';
import { Icon } from '../components/Icon';
import { PageHeader } from '../components/PageHeader';
import { ErrorState, ForbiddenState, LoadingState } from '../components/States';
import { ConfirmDialog, useLoad, WriteAlert, writeError, type WriteError } from './adminKit';
import { JiraWebhookSection } from './JiraWebhook';

const PROVIDER_LABEL: Record<ProviderCode, string> = { jira: 'Jira', lark: 'Lark' };
const PROVIDER_ROLE: Record<ProviderCode, string> = {
  jira: 'Pekerjaan terkait (issue) untuk tiket. Status tiket yang dipetakan ikut diteruskan ke Jira; perubahan di Jira hanya dicatat di timeline, tidak menyelesaikan tiket.',
  lark: 'Notifikasi tim ke grup Lark (nomor tiket dan prioritas saja); tidak mengubah status tiket.',
};
const SECRET_LABEL: Record<ProviderCode, string> = { jira: 'API token Jira', lark: 'App secret Lark' };
const STATE: Record<IntegrationConnection['state'], { label: string; tone: BadgeTone }> = {
  draft: { label: 'Draft', tone: 'neutral' },
  tested: { label: 'Teruji', tone: 'blue' },
  active: { label: 'Aktif', tone: 'ok' },
  disabled: { label: 'Nonaktif', tone: 'waiting' },
};
const ACTION: Record<string, string> = {
  'integration.create': 'Koneksi dibuat',
  'integration.update': 'Nama koneksi diubah',
  'integration.configure': 'Pengaturan disimpan',
  'integration.credential.replace': 'Kredensial disimpan',
  'integration.credential.delete': 'Kredensial dihapus',
  'integration.directory.sync': 'Daftar grup Lark disinkronkan',
  'integration.test': 'Uji koneksi',
  'integration.simulation': 'Hasil simulasi diubah',
  'integration.activate': 'Diaktifkan',
  'integration.disable': 'Dinonaktifkan',
  'integration.webhook_secret.rotate': 'Secret webhook dibuat',
  'integration.webhook_secret.delete': 'Secret webhook dihapus',
  'integration.webhook': 'Kiriman webhook Jira diterapkan',
};
const OUTCOME: Record<string, string> = { success: 'berhasil', denied: 'ditolak', failed: 'gagal' };
const STATUS_LABEL: Record<string, string> = {
  new: 'Baru', open: 'Dibuka', pending_customer: 'Menunggu nasabah', pending_engineering: 'Menunggu engineering',
  resolved: 'Selesai', closed: 'Ditutup',
};

/** Channels with no adapter in this build: shown honestly, nothing to configure. */
const UNAVAILABLE: readonly { name: string; body: string }[] = [
  { name: 'WhatsApp', body: 'Adapter kanal belum dibangun; percakapan hanya tampil baca saja di detail tiket dan tidak ada pengiriman.' },
  { name: 'Email', body: 'Adapter kanal belum dibangun; tidak ada pengiriman atau penerimaan email dari aplikasi ini.' },
  { name: 'Website', body: 'Adapter formulir/website belum dibangun; tiket masuk lewat intake manual.' },
];

/** Integration errors carry their own meaning; the generic 409 text ("diubah pihak lain") would mislead. */
function integrationError(e: unknown, what: string): WriteError {
  if (e instanceof ApiError) {
    const rid = e.requestId ? ` (ID permintaan ${e.requestId})` : '';
    const own = (title: string, body: string): WriteError => ({ title, body: `${body}${rid}`, conflict: false, field: null });
    switch (e.code) {
      case 'keyring_missing': return own('Kunci enkripsi belum dipasang', e.message);
      case 'provider_error': return own('Penyedia menolak atau tidak menjawab', `${e.message} Tidak ada yang berubah.`);
      case 'integration_not_live': return own('Server dalam mode simulasi', e.message);
      case 'integration_not_configured':
        return own('Pengaturan belum lengkap', 'Lengkapi pengaturan dan simpan kredensial dahulu.');
      case 'integration_mode_mismatch':
        return own('Koneksi dari mode lain', 'Koneksi ini disiapkan untuk mode server yang lain. Simpan pengaturan lalu uji ulang.');
      default: break;
    }
  }
  return writeError(e, what);
}

function credentialText(c: IntegrationConnection, tz: string) {
  return c.credential.state === 'stored'
    ? `Tersimpan${c.credential.updatedAt ? ` · diganti ${instantLabel(c.credential.updatedAt, tz)}` : ''}`
    : 'Belum diisi';
}

function targetText(c: IntegrationConnection) {
  const s = c.settings;
  if (c.provider === 'jira') {
    return s.baseUrl && s.projectKey ? `${s.baseUrl.replace(/^https?:\/\//, '')} · proyek ${s.projectKey}` : 'Belum diatur';
  }
  return s.appId ? `App ${s.appId}` : 'Belum diatur';
}

function connectionBadge(c: IntegrationConnection, live: boolean): { label: string; tone: BadgeTone } {
  if (c.mode !== (live ? 'live' : 'simulation')) return { label: 'Perlu disiapkan ulang', tone: 'waiting' };
  if (!live) return c.state === 'active' ? { label: 'Aktif · simulasi', tone: 'blue' } : { label: 'Simulasi', tone: 'neutral' };
  if (c.state === 'active') return { label: 'Tersambung', tone: 'ok' };
  if (c.lastTestOk === false) return { label: 'Uji gagal', tone: 'error' };
  return { label: 'Belum aktif', tone: 'waiting' };
}

/**
 * O08 Integrasi: one card per connection. The server switch `ITSM_INTEGRATIONS` decides whether a test and a
 * delivery reach Jira/Lark (live) or stay inside the app (simulation, for tests/UAT). Secrets are write-only:
 * the page only knows whether one is stored and when it was replaced.
 */
export function IntegrationsPage({ api, workspace, onNotice }: { api: Api; workspace: Workspace; onNotice: (t: string) => void }) {
  const res = useLoad((s) => api.listIntegrations(workspace.id, s), workspace.id);
  const [open, setOpen] = useState<string | null>(null);
  const [creating, setCreating] = useState<ProviderCode | null>(null);
  const header = (
    <PageHeader title="Integrasi"
      description={`Jira & Lark · satu koneksi per penyedia per perusahaan ${workspace.code}`} />
  );
  if (res.error) {
    if (res.error instanceof ApiError && res.error.status === 403) return <>{header}<ForbiddenState /></>;
    return (
      <>
        {header}
        <ErrorState title="Integrasi belum dapat dimuat" body={res.error instanceof ApiError ? res.error.message : 'Terjadi kesalahan.'}
          requestId={res.error instanceof ApiError ? res.error.requestId : null} onRetry={res.reload} />
      </>
    );
  }
  const list = res.data;
  if (!list) return <>{header}<LoadingState label="Memuat integrasi…" /></>;
  const live = list.integrationsMode === 'live';
  const replace = (c: IntegrationConnection) => res.set({ ...list, items: list.items.map((x) => (x.id === c.id ? c : x)) });
  const target = list.items.find((c) => c.id === open) ?? null;
  const tz = workspace.timezone;
  return (
    <>
      {header}
      {!live && (
        <p className="banner spaced-bottom" role="note">
          Server berjalan dalam <strong>mode simulasi</strong> (ITSM_INTEGRATIONS=simulation) untuk uji/UAT: uji koneksi dan
          pengiriman tidak menghubungi Jira atau Lark.
        </p>
      )}
      {live && !list.keyringConfigured && (
        <section className="pcard callout-amber" aria-labelledby="int-keyring">
          <div className="bd">
            <h2 id="int-keyring">Kunci enkripsi server belum dipasang</h2>
            <p className="hintline">ITSM_KEYRING belum diatur: kredensial tidak dapat disimpan dan koneksi tidak dapat diaktifkan. Hubungi admin server.</p>
          </div>
        </section>
      )}
      <div className="int-grid">
        {list.items.map((c) => {
          const badge = connectionBadge(c, live);
          return (
            <section key={c.id} className="pcard int-card" aria-labelledby={`int-${c.id}`}>
              <div className="hd">
                <span className="int-logo" aria-hidden="true">{PROVIDER_LABEL[c.provider].slice(0, 1)}</span>
                <h2 id={`int-${c.id}`}>{PROVIDER_LABEL[c.provider]}</h2>
                <Badge tone={badge.tone}>{badge.label}</Badge>
                <div className="r">
                  <Button className="btn-small" onClick={() => setOpen(c.id)} aria-label={`Kelola ${PROVIDER_LABEL[c.provider]}`}>Kelola</Button>
                </div>
              </div>
              <div className="bd">
                <p className="int-role">{PROVIDER_ROLE[c.provider]}</p>
                <dl className="kv int-kv">
                  <dt>Konfigurasi</dt><dd><Badge tone={STATE[c.state].tone}>{STATE[c.state].label}</Badge></dd>
                  <dt>Tujuan</dt><dd className="kv-text">{targetText(c)}</dd>
                  <dt>Kredensial</dt><dd className="kv-text">{credentialText(c, tz)}</dd>
                  <dt>Terakhir diuji</dt>
                  <dd className="kv-text">
                    {c.testedAt ? `${instantLabel(c.testedAt, tz)}${c.lastTestOk === false ? ' · gagal' : c.lastTestOk ? ' · berhasil' : ''}` : 'Belum pernah diuji'}
                  </dd>
                  <dt>{c.provider === 'jira' ? 'Assignee' : 'Grup tujuan'}</dt>
                  <dd className="kv-text">{c.directoryCount} {c.provider === 'jira' ? 'dikenal' : 'grup'}</dd>
                </dl>
              </div>
            </section>
          );
        })}
        {list.creatable.map((p) => (
          <section key={p} className="pcard int-card" aria-labelledby={`int-new-${p}`}>
            <div className="hd">
              <span className="int-logo" aria-hidden="true">{PROVIDER_LABEL[p].slice(0, 1)}</span>
              <h2 id={`int-new-${p}`}>{PROVIDER_LABEL[p]}</h2>
              <Badge>Belum dibuat</Badge>
              <div className="r"><Button className="btn-small" variant="primary" onClick={() => setCreating(p)}><Icon name="plus" size={14} /> Buat koneksi</Button></div>
            </div>
            <div className="bd">
              <p className="int-role">{PROVIDER_ROLE[p]}</p>
              <p className="empty-mini">Belum ada koneksi {PROVIDER_LABEL[p]} di perusahaan ini. Koneksi baru dimulai sebagai draft.</p>
            </div>
          </section>
        ))}
        <section className="pcard int-card" aria-labelledby="cif-title">
          <div className="hd">
            <span className="int-logo" aria-hidden="true"><Icon name="user" size={15} /></span>
            <h2 id="cif-title">Customer Information File (CIF)</h2>
            <Badge tone="waiting">Belum tersambung</Badge>
          </div>
          <div className="bd">
            <p className="int-role">Satu referensi nasabah untuk penanganan tiket yang lebih tepat.</p>
            <dl className="kv int-kv">
              <dt>Adapter</dt><dd className="kv-text">Belum tersedia</dd>
              <dt>Referensi nasabah</dt><dd className="kv-text">Belum diverifikasi ke sistem CIF</dd>
            </dl>
            <details className="int-more">
              <summary>Rancangan alur CIF &amp; batas akses</summary>
              <ol className="cif-flow">
                <li><strong>Cari dalam perusahaan aktif</strong><span>Gunakan referensi CIF atau pencarian terbatas; hasil dari perusahaan lain tidak ditampilkan.</span></li>
                <li><strong>Cocokkan nasabah</strong><span>Petugas memeriksa identitas tersamarkan sebelum memilih. Hindari menyalin data sensitif ke judul atau percakapan tiket.</span></li>
                <li><strong>Tautkan referensi ke tiket</strong><span>Simpan ID sumber dan waktu verifikasi. Jika CIF tidak tersedia, tandai belum terverifikasi; jangan membuat identitas baru otomatis.</span></li>
              </ol>
              <p className="hintline">Aktivasi memerlukan kontrak API, pemetaan akses per perusahaan, dan pengujian isolasi data.</p>
            </details>
          </div>
        </section>
      </div>

      <section className="pcard int-channels" aria-labelledby="int-channels">
        <div className="hd"><h2 id="int-channels">Kanal layanan</h2><span className="sub">Kesiapan adapter</span></div>
        <ul className="plain int-channel-list">
          {UNAVAILABLE.map((u) => (
            <li key={u.name}>
              <div className="int-ch-hd"><strong>{u.name}</strong> <Badge>Belum tersedia</Badge></div>
              <p>{u.body}</p>
            </li>
          ))}
        </ul>
      </section>
      {creating && (
        <CreateDialog api={api} workspace={workspace} provider={creating} onClose={() => setCreating(null)}
          onDone={(c) => {
            res.set({ ...list, items: [...list.items, c], creatable: list.creatable.filter((p) => p !== c.provider) });
            setCreating(null);
            setOpen(c.id);
            onNotice(`Koneksi ${PROVIDER_LABEL[c.provider]} dibuat sebagai draft. Tercatat di audit.`);
          }} />
      )}
      {target && (
        <ConnectionDialog api={api} workspace={workspace} list={list} connection={target} onClose={() => setOpen(null)}
          onChanged={replace} onReload={() => { setOpen(null); res.reload(); }} onNotice={onNotice} />
      )}
    </>
  );
}

function CreateDialog({ api, workspace, provider, onClose, onDone }: {
  api: Api; workspace: Workspace; provider: ProviderCode; onClose: () => void; onDone: (c: IntegrationConnection) => void;
}) {
  const [name, setName] = useState(`${workspace.code} ${PROVIDER_LABEL[provider]}`);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const formId = useId();
  async function submit() {
    if (busy || !name.trim()) return;
    setBusy(true);
    setError(null);
    try {
      onDone((await api.createIntegration(workspace.id, { provider, displayName: name.trim() })).item);
    } catch (e) {
      setBusy(false);
      setError(writeError(e, 'Koneksi'));
    }
  }
  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title={`Buat koneksi ${PROVIDER_LABEL[provider]}`} initialFocus="input"
      footer={(
        <>
          <Button onClick={onClose} disabled={busy}>Batal</Button>
          <Button type="submit" form={formId} variant="primary" disabled={busy || !name.trim()} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan draft'}</Button>
        </>
      )}>
      {error && <WriteAlert error={error} />}
      <form id={formId} onSubmit={(e) => { e.preventDefault(); void submit(); }}>
        <TextField label="Nama koneksi" required maxLength={200} value={name} disabled={busy} onChange={(e) => setName(e.target.value)} />
      </form>
      <p className="tiny muted spaced">Koneksi baru berstatus draft. Alamat, kredensial dan mapping diisi setelah draft tersimpan.</p>
    </Dialog>
  );
}

type Step = 'test' | 'activate' | 'disable';
type Busy = null | 'name' | 'settings' | 'secret' | 'step' | 'options' | 'sync';

interface SettingsDraft {
  baseUrl: string; email: string; projectKey: string; issueTypeId: string; appId: string;
  priorityMap: Record<string, string>; statusMap: Record<string, string>;
}

function draftOf(c: IntegrationConnection): SettingsDraft {
  const s = c.settings;
  return {
    baseUrl: s.baseUrl ?? '', email: s.email ?? '', projectKey: s.projectKey ?? '', issueTypeId: s.issueTypeId ?? '',
    appId: s.appId ?? '', priorityMap: { ...s.priorityMap }, statusMap: { ...s.statusMap },
  };
}

const FIELD_ERROR: Record<string, string> = {
  baseUrl: 'Alamat harus https ke penyedia resmi, tanpa port atau path: Jira https://<nama>.atlassian.net; Lark https://open.larksuite.com atau https://open.feishu.cn.',
  email: 'Email akun Jira tidak valid.',
  projectKey: 'Project key 2–31 huruf besar/angka, diawali huruf (mis. OPS).',
  issueTypeId: 'ID tipe isu berupa angka dari Jira.',
  appId: 'App ID Lark berupa huruf, angka, garis bawah atau tanda hubung (mis. cli_a1b2c3).',
  priorityMap: 'ID prioritas Jira berupa angka.',
  statusMap: 'ID status Jira berupa angka.',
};

function MapSelect({ label, value, options, onChange, disabled }: {
  label: string; value: string; options: IntegrationIdName[] | null; onChange: (v: string) => void; disabled: boolean;
}) {
  if (!options) {
    return <TextField label={label} value={value} inputMode="numeric" maxLength={32} disabled={disabled} placeholder="ID dari Jira"
      onChange={(e) => onChange(e.target.value)} />;
  }
  const known = value === '' || options.some((o) => o.id === value);
  return (
    <SelectField label={label} value={value} disabled={disabled} onChange={(e) => onChange(e.target.value)}>
      <option value="">— tidak dipetakan —</option>
      {!known && <option value={value}>ID {value} (tidak ditemukan di Jira)</option>}
      {options.map((o) => <option key={o.id} value={o.id}>{o.name}</option>)}
    </SelectField>
  );
}

function ConnectionDialog({ api, workspace, list, connection: c, onClose, onChanged, onReload, onNotice }: {
  api: Api; workspace: Workspace; list: IntegrationList; connection: IntegrationConnection;
  onClose: () => void; onChanged: (c: IntegrationConnection) => void; onReload: () => void; onNotice: (t: string) => void;
}) {
  const [name, setName] = useState(c.displayName);
  const [draft, setDraft] = useState<SettingsDraft>(() => draftOf(c));
  const [secret, setSecret] = useState('');
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<WriteError | null>(null);
  const [confirm, setConfirm] = useState<null | 'disable' | 'deleteSecret'>(null);
  const [options, setOptions] = useState<JiraOptions | null>(null);
  const label = PROVIDER_LABEL[c.provider];
  const live = list.integrationsMode === 'live';
  const tz = workspace.timezone;
  const settingsId = useId();
  const secretId = useId();
  const stored = c.credential.state === 'stored';

  async function run(kind: Busy, what: string, f: () => Promise<{ item: IntegrationConnection }>, notice: (n: IntegrationConnection) => string) {
    setBusy(kind);
    setError(null);
    try {
      const r = await f();
      onChanged(r.item);
      onNotice(notice(r.item));
      return r.item;
    } catch (e) {
      setError(integrationError(e, what));
      return null;
    } finally {
      setBusy(null);
      setConfirm(null);
    }
  }
  const step = (s: Step) => run('step', 'Status koneksi', () => api.stepIntegration(workspace.id, c.id, s, { version: c.version }),
    (n) => s === 'test'
      ? `${label}: uji ${n.lastTestOk ? 'berhasil' : 'gagal'}${n.lastTestNote ? ` — ${n.lastTestNote}` : ''}`
      : `${label}: ${STATE[n.state].label}. Tercatat di audit.`);
  const set = <K extends keyof SettingsDraft>(k: K, v: SettingsDraft[K]) => setDraft((d) => ({ ...d, [k]: v }));
  const mapSet = (k: 'priorityMap' | 'statusMap', code: string, v: string) =>
    setDraft((d) => ({ ...d, [k]: { ...d[k], [code]: v } }));

  async function saveSettings() {
    const clean = (m: Record<string, string>) => Object.fromEntries(Object.entries(m).filter(([, v]) => v.trim() !== ''));
    const body = c.provider === 'jira'
      ? {
        baseUrl: draft.baseUrl.trim(), email: draft.email.trim(), projectKey: draft.projectKey.trim(),
        issueTypeId: draft.issueTypeId.trim(), priorityMap: clean(draft.priorityMap), statusMap: clean(draft.statusMap), version: c.version,
      }
      : { appId: draft.appId.trim(), baseUrl: draft.baseUrl.trim() || null, version: c.version };
    const n = await run('settings', 'Pengaturan', () => api.configureIntegration(workspace.id, c.id, body),
      (n) => `Pengaturan ${label} tersimpan${stored && n.credential.state !== 'stored'
        ? `; ${SECRET_LABEL[c.provider]} dihapus karena alamat atau akun berubah, masukkan ulang`
        : n.state === 'draft' && c.state !== 'draft' ? '; uji ulang sebelum diaktifkan' : ''}. Tercatat di audit.`);
    if (n) setDraft(draftOf(n));
  }
  async function saveSecret() {
    if (!secret) return;
    const n = await run('secret', 'Kredensial', () => api.putIntegrationCredential(workspace.id, c.id, { secret, version: c.version }),
      () => `${SECRET_LABEL[c.provider]} tersimpan terenkripsi; uji koneksi sebelum diaktifkan. Tercatat di audit (tanpa nilai).`);
    if (n) setSecret('');
  }
  async function loadOptions() {
    setBusy('options');
    setError(null);
    try {
      setOptions(await api.getJiraOptions(workspace.id, c.id));
    } catch (e) {
      setError(integrationError(e, 'Data Jira'));
    } finally {
      setBusy(null);
    }
  }
  async function syncChats() {
    setBusy('sync');
    setError(null);
    try {
      const r = await api.syncLarkChats(workspace.id, c.id);
      onChanged(r.item);
      onNotice(`Grup Lark disinkronkan: ${r.synced} grup tersedia untuk intake${r.deactivated ? `, ${r.deactivated} tidak lagi ditawarkan` : ''}.`);
    } catch (e) {
      setError(integrationError(e, 'Daftar grup'));
    } finally {
      setBusy(null);
    }
  }

  const modeOk = c.mode === list.integrationsMode;
  const ready = c.provider === 'jira'
    ? !!(c.settings.baseUrl && c.settings.email && c.settings.projectKey)
    : !!c.settings.appId;
  const liveBlock = live && (!list.keyringConfigured || !stored || c.lastTestOk !== true);
  const canActivate = c.state === 'tested' && modeOk && !liveBlock;
  const canDisable = c.state !== 'disabled';
  const dirty = JSON.stringify(draft) !== JSON.stringify(draftOf(c));
  const retarget = stored && credentialRetargeted(c.provider, draftOf(c), draft);
  const steps: { id: string; label: string; done: boolean }[] = [
    { id: 'save', label: '1. Atur', done: ready && (!live || stored) },
    { id: 'test', label: '2. Uji', done: (c.state === 'tested' || c.state === 'active') && c.lastTestOk !== false },
    { id: 'activate', label: '3. Aktifkan', done: c.state === 'active' },
  ];
  const activateHint = !modeOk ? 'Simpan pengaturan lalu uji ulang di mode server saat ini'
    : live && !list.keyringConfigured ? 'Kunci enkripsi server belum dipasang'
      : live && !stored ? 'Simpan kredensial dahulu'
        : c.state !== 'tested' ? 'Uji koneksi dahulu' : undefined;
  const disabled = busy !== null;
  return (
    <>
      <Dialog open onClose={disabled ? () => undefined : onClose} title={`Konfigurasi ${label}`} variant="peek" initialFocus="input"
        footer={(
          <>
            <Button onClick={onClose} disabled={disabled}>Tutup</Button>
            {canDisable && <Button disabled={disabled} onClick={() => setConfirm('disable')}>Nonaktifkan</Button>}
            <Button disabled={disabled || !ready || dirty} aria-busy={busy === 'step'} onClick={() => void step('test')}
              title={dirty ? 'Simpan pengaturan dahulu' : ready ? undefined : 'Lengkapi pengaturan dahulu'}>
              {busy === 'step' ? 'Menguji…' : 'Uji koneksi'}
            </Button>
            <Button variant="primary" disabled={disabled || !canActivate} onClick={() => void step('activate')} title={activateHint}>Aktifkan</Button>
          </>
        )}>
        <ol className="steps plain row" aria-label="Urutan aktivasi">
          {steps.map((s) => <li key={s.id} aria-current={s.done ? undefined : 'step'}>{s.done ? <strong>{s.label} ✓</strong> : <span>{s.label}</span>}</li>)}
        </ol>
        <p className="banner spaced-bottom">
          {workspace.code} · konfigurasi <strong>{STATE[c.state].label}</strong> ·{' '}
          {live
            ? `uji koneksi menghubungi ${label} langsung dari server.`
            : `mode simulasi: uji dan pengiriman tidak menghubungi ${label}.`}
          {!modeOk && ' Koneksi ini disiapkan di mode lain; simpan pengaturan dan uji ulang.'}
        </p>
        {error && <WriteAlert error={error} onReload={onReload} />}
        <form className="row spaced-bottom" onSubmit={(e) => {
          e.preventDefault();
          if (!disabled && name.trim() && name.trim() !== c.displayName) {
            void run('name', 'Nama koneksi', () => api.updateIntegration(workspace.id, c.id, { displayName: name.trim(), version: c.version }),
              () => `Nama koneksi ${label} tersimpan. Tercatat di audit.`);
          }
        }}>
          <TextField label="Nama koneksi" required maxLength={200} value={name} disabled={disabled}
            error={error?.field === 'displayName' ? 'Periksa nama koneksi.' : undefined} onChange={(e) => setName(e.target.value)} />
          <Button type="submit" disabled={disabled || !name.trim() || name.trim() === c.displayName}>Simpan nama</Button>
        </form>

        <section className="int-section" aria-labelledby={`${settingsId}-h`}>
          <h3 id={`${settingsId}-h`}>Pengaturan {label}</h3>
          <form id={settingsId} className="int-form" onSubmit={(e) => { e.preventDefault(); if (!disabled && dirty) void saveSettings(); }}>
            {c.provider === 'jira' ? (
              <>
                <TextField label="Alamat Jira" required type="url" inputMode="url" maxLength={200} value={draft.baseUrl} disabled={disabled}
                  placeholder="https://kantor.atlassian.net" autoComplete="off"
                  error={error?.field === 'baseUrl' ? FIELD_ERROR.baseUrl : undefined} onChange={(e) => set('baseUrl', e.target.value)} />
                <TextField label="Email akun Jira" required type="email" maxLength={254} value={draft.email} disabled={disabled} autoComplete="off"
                  hint="Akun layanan pemilik API token."
                  error={error?.field === 'email' ? FIELD_ERROR.email : undefined} onChange={(e) => set('email', e.target.value)} />
                <TextField label="Project key" required maxLength={31} value={draft.projectKey} disabled={disabled} autoComplete="off"
                  placeholder="OPS" error={error?.field === 'projectKey' ? FIELD_ERROR.projectKey : undefined}
                  onChange={(e) => set('projectKey', e.target.value.toUpperCase())} />
                {options ? (
                  <SelectField label="Tipe isu" value={draft.issueTypeId} disabled={disabled}
                    error={error?.field === 'issueTypeId' ? FIELD_ERROR.issueTypeId : undefined} onChange={(e) => set('issueTypeId', e.target.value)}>
                    <option value="">— pilih tipe isu —</option>
                    {draft.issueTypeId && !options.issueTypes.some((t) => t.id === draft.issueTypeId) &&
                      <option value={draft.issueTypeId}>ID {draft.issueTypeId} (tidak ditemukan di proyek)</option>}
                    {options.issueTypes.map((t) => <option key={t.id} value={t.id}>{t.name}</option>)}
                  </SelectField>
                ) : (
                  <TextField label="Tipe isu (ID)" inputMode="numeric" maxLength={32} value={draft.issueTypeId} disabled={disabled}
                    hint={live ? 'Muat dari Jira untuk memilih dari daftar.' : undefined}
                    error={error?.field === 'issueTypeId' ? FIELD_ERROR.issueTypeId : undefined} onChange={(e) => set('issueTypeId', e.target.value)} />
                )}
              </>
            ) : (
              <>
                <TextField label="App ID" required maxLength={64} value={draft.appId} disabled={disabled} autoComplete="off" placeholder="cli_…"
                  hint="Aplikasi bot internal (custom app) di Lark Developer Console."
                  error={error?.field === 'appId' ? FIELD_ERROR.appId : undefined} onChange={(e) => set('appId', e.target.value)} />
                <TextField label="Alamat API Lark (opsional)" type="url" inputMode="url" maxLength={200} value={draft.baseUrl} disabled={disabled}
                  placeholder="https://open.larksuite.com" autoComplete="off" hint="Kosongkan untuk Lark internasional."
                  error={error?.field === 'baseUrl' ? FIELD_ERROR.baseUrl : undefined} onChange={(e) => set('baseUrl', e.target.value)} />
              </>
            )}
          </form>
          {retarget && (
            <p className="hintline" role="status">
              Alamat, email atau App ID berubah: {SECRET_LABEL[c.provider]} yang tersimpan akan dihapus saat pengaturan disimpan dan
              harus dimasukkan ulang oleh pemegang hak kelola kredensial.
            </p>
          )}
          {c.provider === 'jira' && (
            <fieldset className="int-map">
              <legend>Pemetaan prioritas &amp; status</legend>
              <p className="tiny muted">
                Prioritas tiket dipakai saat issue dibuat. Pemetaan status disimpan sebagai acuan; status Jira tidak diubah otomatis.
              </p>
              <div className="int-form">
                {list.priorities.map((p) => (
                  <MapSelect key={`p-${p.code}`} label={`Prioritas ${p.label}`} value={draft.priorityMap[p.code] ?? ''}
                    options={options?.priorities ?? null} disabled={disabled} onChange={(v) => mapSet('priorityMap', p.code, v)} />
                ))}
                {list.ticketStatuses.map((s) => (
                  <MapSelect key={`s-${s}`} label={`Status ${STATUS_LABEL[s] ?? s}`} value={draft.statusMap[s] ?? ''}
                    options={options?.statuses ?? null} disabled={disabled} onChange={(v) => mapSet('statusMap', s, v)} />
                ))}
              </div>
              {(error?.field === 'priorityMap' || error?.field === 'statusMap') &&
                <p className="field-error">{FIELD_ERROR[error.field]}</p>}
            </fieldset>
          )}
          <div className="row spaced-bottom">
            <Button type="submit" form={settingsId} variant="primary" disabled={disabled || !dirty} aria-busy={busy === 'settings'}>
              {busy === 'settings' ? 'Menyimpan…' : 'Simpan pengaturan'}
            </Button>
            {live && c.provider === 'jira' && (
              <Button disabled={disabled || dirty || !ready || !stored} aria-busy={busy === 'options'} onClick={() => void loadOptions()}
                title={!stored ? 'Simpan kredensial dahulu' : dirty ? 'Simpan pengaturan dahulu' : undefined}>
                {busy === 'options' ? 'Memuat…' : options ? 'Muat ulang dari Jira' : 'Muat tipe isu & prioritas dari Jira'}
              </Button>
            )}
            {live && c.provider === 'lark' && (
              <Button disabled={disabled || dirty || !ready || !stored} aria-busy={busy === 'sync'} onClick={() => void syncChats()}
                title={!stored ? 'Simpan kredensial dahulu' : undefined}>
                {busy === 'sync' ? 'Menyinkronkan…' : 'Sinkronkan grup Lark'}
              </Button>
            )}
          </div>
        </section>

        <section className="int-section" aria-labelledby={`${secretId}-h`}>
          <h3 id={`${secretId}-h`}>Kredensial</h3>
          <dl className="props spaced-bottom">
            <div className="props-row"><dt>{SECRET_LABEL[c.provider]}</dt><dd>{credentialText(c, tz)}</dd></div>
          </dl>
          {list.canManageCredentials ? (
            <>
              <form className="row spaced-bottom" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (!disabled) void saveSecret(); }}>
                <TextField label={stored ? `Ganti ${SECRET_LABEL[c.provider]}` : SECRET_LABEL[c.provider]} type="password" autoComplete="new-password"
                  maxLength={4000} value={secret} disabled={disabled || (live && !list.keyringConfigured)} spellCheck={false}
                  hint="Disimpan terenkripsi di server dan tidak pernah ditampilkan lagi."
                  error={error?.field === 'secret' ? 'Nilai tidak valid (1–4000 karakter, tanpa baris baru).' : undefined}
                  onChange={(e) => setSecret(e.target.value)} />
                <Button type="submit" disabled={disabled || !secret} aria-busy={busy === 'secret'}>
                  {busy === 'secret' ? 'Menyimpan…' : stored ? 'Ganti kredensial' : 'Simpan kredensial'}
                </Button>
              </form>
              {stored && <Button disabled={disabled} onClick={() => setConfirm('deleteSecret')}>Hapus kredensial</Button>}
            </>
          ) : (
            <p className="tiny muted">Anda tidak memegang hak kelola kredensial integrasi; minta admin perusahaan.</p>
          )}
        </section>

        {c.provider === 'jira' && (
          <JiraWebhookSection api={api} workspace={workspace} list={list} connection={c}
            onChanged={onChanged} onNotice={onNotice} toError={integrationError} />
        )}

        <dl className="props spaced-bottom">
          <div className="props-row">
            <dt>Hasil uji terakhir</dt>
            <dd>
              {c.lastTestOk === false && <><Badge tone="error">Gagal</Badge>{' '}</>}
              {c.lastTestOk === true && <><Badge tone="ok">Berhasil</Badge>{' '}</>}
              {c.lastTestNote ?? 'Belum pernah diuji.'}{c.testedAt ? ` (${instantLabel(c.testedAt, tz)})` : ''}
            </dd>
          </div>
          <div className="props-row">
            <dt>{c.provider === 'jira' ? 'Assignee dikenal' : 'Grup tujuan'}</dt>
            <dd>{c.directoryCount}{c.provider === 'jira' ? ' (bertambah dari pencarian pengguna Jira saat intake)' : ' grup'}</dd>
          </div>
        </dl>
        <section aria-labelledby={`log-${c.id}`}>
          <h3 id={`log-${c.id}`}>Log konfigurasi terbaru</h3>
          {c.recent.length === 0 ? <p className="tiny muted">Belum ada perubahan tercatat.</p> : (
            <ol className="plain history">
              {c.recent.map((r, i) => (
                <li key={`${r.occurredAt}-${i}`} className="activity">
                  <strong>{ACTION[r.action] ?? r.action}</strong>
                  <span className="tiny muted"> · {instantLabel(r.occurredAt, tz)}{r.actor ? ` · oleh ${r.actor}` : ''} · {OUTCOME[r.outcome] ?? r.outcome}</span>
                </li>
              ))}
            </ol>
          )}
        </section>
      </Dialog>
      {confirm === 'disable' && (
        <ConfirmDialog title={`Nonaktifkan ${label}?`} confirmLabel="Nonaktifkan"
          impact={`Pengiriman ${label} yang masih antre di ${workspace.code} akan gagal dengan alasan "koneksi tidak aktif" dan tidak dikirim. Tiket tidak berubah; aktifkan lagi lalu kirim ulang dari detail tiket.`}
          onClose={() => setConfirm(null)} onConfirm={async () => { await step('disable'); }} />
      )}
      {confirm === 'deleteSecret' && (
        <ConfirmDialog title={`Hapus ${SECRET_LABEL[c.provider]}?`} confirmLabel="Hapus kredensial"
          impact={`Koneksi kembali ke draft dan ${label} tidak menerima kiriman sampai kredensial baru disimpan dan diuji.`}
          onClose={() => setConfirm(null)}
          onConfirm={async () => {
            await run('secret', 'Kredensial', () => api.deleteIntegrationCredential(workspace.id, c.id, { version: c.version }),
              () => `${SECRET_LABEL[c.provider]} dihapus. Tercatat di audit.`);
          }} />
      )}
    </>
  );
}
