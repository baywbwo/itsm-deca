import { useId, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type { IntegrationConnection, IntegrationList, ProviderCode, Workspace } from '../api/types';
import { instantLabel } from '../app/format';
import { Badge, type BadgeTone } from '../components/Badge';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { TextField } from '../components/Field';
import { Icon } from '../components/Icon';
import { PageHeader } from '../components/PageHeader';
import { ErrorState, ForbiddenState, LoadingState } from '../components/States';
import { ConfirmDialog, useLoad, WriteAlert, writeError, type WriteError } from './adminKit';

const PROVIDER_LABEL: Record<ProviderCode, string> = { jira: 'Jira', lark: 'Lark' };
const PROVIDER_ROLE: Record<ProviderCode, string> = {
  jira: 'Pekerjaan terkait (issue) untuk tiket; status Jira Done tidak otomatis menyelesaikan tiket.',
  lark: 'Notifikasi tim; tidak mengubah status tiket.',
};
const STATE: Record<IntegrationConnection['state'], { label: string; tone: BadgeTone }> = {
  draft: { label: 'Draft', tone: 'neutral' },
  tested: { label: 'Teruji', tone: 'blue' },
  active: { label: 'Aktif', tone: 'ok' },
  disabled: { label: 'Nonaktif', tone: 'waiting' },
};
const ACTION: Record<string, string> = {
  'integration.create': 'Koneksi dibuat',
  'integration.update': 'Nama koneksi diubah',
  'integration.test': 'Uji konfigurasi',
  'integration.simulation': 'Percobaan kirim (tanpa penyedia)',
  'integration.activate': 'Diaktifkan',
  'integration.disable': 'Dinonaktifkan',
};

const OUTCOME: Record<string, string> = { success: 'berhasil', denied: 'ditolak', failed: 'gagal' };

/** Channels with no adapter in this build: shown honestly, nothing to configure. */
const UNAVAILABLE: readonly { name: string; body: string }[] = [
  { name: 'WhatsApp', body: 'Adapter kanal belum dibangun; percakapan hanya tampil baca saja di detail tiket dan tidak ada pengiriman.' },
  { name: 'Email', body: 'Adapter kanal belum dibangun; tidak ada pengiriman atau penerimaan email dari aplikasi ini.' },
  { name: 'Website', body: 'Adapter formulir/website belum dibangun; tiket masuk lewat intake manual.' },
];

/**
 * O08 Integrasi: one card per connection. Every connection is `mode: simulation` in this build: save → test →
 * activate, disable at any time; no provider is contacted and no credential or mapping exists, so each card
 * says "Belum tersambung". Page copy avoids "simulasi"/"dummy"; names and notes typed into the data are shown as stored.
 */
export function IntegrationsPage({ api, workspace, onNotice }: { api: Api; workspace: Workspace; onNotice: (t: string) => void }) {
  const res = useLoad((s) => api.listIntegrations(workspace.id, s), workspace.id);
  const [open, setOpen] = useState<string | null>(null);
  const [creating, setCreating] = useState<ProviderCode | null>(null);
  const header = (
    <PageHeader title="Integrasi"
      description={`Jira & Lark · satu koneksi per penyedia per workspace ${workspace.code}`} />
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
  const replace = (c: IntegrationConnection) => res.set({ ...list, items: list.items.map((x) => (x.id === c.id ? c : x)) });
  const target = list.items.find((c) => c.id === open) ?? null;
  const tz = workspace.timezone;
  return (
    <>
      {header}
      <div className="int-grid">
        {list.items.map((c) => (
          <section key={c.id} className="pcard int-card" aria-labelledby={`int-${c.id}`}>
            <div className="hd">
              <span className="int-logo" aria-hidden="true">{PROVIDER_LABEL[c.provider].slice(0, 1)}</span>
              <h2 id={`int-${c.id}`}>{PROVIDER_LABEL[c.provider]}</h2>
              <Badge tone="waiting">Belum tersambung</Badge>
              <div className="r">
                <Button className="btn-small" onClick={() => setOpen(c.id)} aria-label={`Kelola ${PROVIDER_LABEL[c.provider]}`}>Kelola</Button>
              </div>
            </div>
            <div className="bd">
              <p className="int-role">{PROVIDER_ROLE[c.provider]}</p>
              <dl className="kv int-kv">
                <dt>Konfigurasi</dt><dd><Badge tone={STATE[c.state].tone}>{STATE[c.state].label}</Badge></dd>
                <dt>Kredensial</dt><dd className="kv-text">Belum tersedia</dd>
                <dt>Mapping</dt><dd className="kv-text">Belum tersedia</dd>
                <dt>Terakhir diuji</dt><dd className="kv-text">{c.testedAt ? instantLabel(c.testedAt, tz) : 'Belum pernah diuji'}</dd>
                <dt>Daftar tujuan</dt><dd className="kv-text">{c.directoryCount} entri uji</dd>
              </dl>
            </div>
          </section>
        ))}
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
              <p className="empty-mini">Belum ada koneksi {PROVIDER_LABEL[p]} di workspace ini. Koneksi baru dimulai sebagai draft.</p>
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

      <section className="pcard callout-amber" aria-labelledby="int-note">
        <div className="bd">
          <h2 id="int-note">Koneksi belum mengirim ke penyedia</h2>
          <p className="hintline">
            Kredensial dan mapping penyedia belum tersedia (menunggu endpoint, akun dan kode terverifikasi dari kantor).
            Uji konfigurasi hanya memeriksa kelengkapan di aplikasi ini; tidak ada request ke Jira atau Lark dan hasilnya tidak membuktikan koneksi kantor.
          </p>
        </div>
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
      <p className="tiny muted spaced">Koneksi baru berstatus draft. Endpoint, kredensial dan mapping tidak diisi di sini.</p>
    </Dialog>
  );
}

type Step = 'test' | 'activate' | 'disable';

function ConnectionDialog({ api, workspace, list, connection: c, onClose, onChanged, onReload, onNotice }: {
  api: Api; workspace: Workspace; list: IntegrationList; connection: IntegrationConnection;
  onClose: () => void; onChanged: (c: IntegrationConnection) => void; onReload: () => void; onNotice: (t: string) => void;
}) {
  const [name, setName] = useState(c.displayName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const [confirmOff, setConfirmOff] = useState(false);
  const label = PROVIDER_LABEL[c.provider];

  async function run(what: string, f: () => Promise<{ item: IntegrationConnection }>, notice: (n: IntegrationConnection) => string) {
    setBusy(true);
    setError(null);
    try {
      const r = await f();
      onChanged(r.item);
      onNotice(notice(r.item));
    } catch (e) {
      setError(writeError(e, what));
    } finally {
      setBusy(false);
      setConfirmOff(false);
    }
  }
  const step = (s: Step) => run('Status koneksi', () => api.stepIntegration(workspace.id, c.id, s, { version: c.version }),
    (n) => `${label}: ${STATE[n.state].label}. Tercatat di audit.`);
  const canTest = c.state !== 'active';
  const canActivate = c.state === 'tested';
  const canDisable = c.state !== 'disabled';
  const steps: { id: string; label: string; done: boolean }[] = [
    { id: 'save', label: '1. Simpan', done: true },
    { id: 'test', label: '2. Uji', done: c.state === 'tested' || c.state === 'active' },
    { id: 'activate', label: '3. Aktifkan', done: c.state === 'active' },
  ];
  return (
    <>
      <Dialog open onClose={busy ? () => undefined : onClose} title={`Konfigurasi ${label}`} variant="peek" initialFocus="input"
        footer={(
          <>
            <Button onClick={onClose} disabled={busy}>Tutup</Button>
            {canDisable && <Button disabled={busy} onClick={() => setConfirmOff(true)}>Nonaktifkan</Button>}
            <Button disabled={busy || !canTest} onClick={() => void step('test')}
              title={canTest ? undefined : 'Koneksi aktif; nonaktifkan dahulu untuk menguji ulang'}>Uji konfigurasi</Button>
            <Button variant="primary" disabled={busy || !canActivate} onClick={() => void step('activate')}
              title={canActivate ? undefined : 'Uji konfigurasi dahulu'}>Aktifkan</Button>
          </>
        )}>
        <ol className="steps plain row" aria-label="Urutan aktivasi">
          {steps.map((s) => <li key={s.id} aria-current={s.done ? undefined : 'step'}>{s.done ? <strong>{s.label} ✓</strong> : <span>{s.label}</span>}</li>)}
        </ol>
        <p className="banner spaced-bottom">Workspace {workspace.code} · konfigurasi <strong>{STATE[c.state].label}</strong> · belum tersambung; tidak ada request jaringan ke {label}.</p>
        {error && <WriteAlert error={error} onReload={onReload} />}
        <form className="row spaced-bottom" onSubmit={(e) => {
          e.preventDefault();
          if (!busy && name.trim() && name.trim() !== c.displayName) {
            void run('Nama koneksi', () => api.updateIntegration(workspace.id, c.id, { displayName: name.trim(), version: c.version }),
              () => `Nama koneksi ${label} tersimpan. Tercatat di audit.`);
          }
        }}>
          <TextField label="Nama koneksi" required maxLength={200} value={name} disabled={busy}
            error={error?.field === 'displayName' ? 'Periksa nama koneksi.' : undefined} onChange={(e) => setName(e.target.value)} />
          <Button type="submit" disabled={busy || !name.trim() || name.trim() === c.displayName}>Simpan nama</Button>
        </form>
        <dl className="props spaced-bottom">
          <div className="props-row"><dt>Kredensial</dt><dd>Belum tersedia — penyimpanan kredensial aman belum ada{list.canManageCredentials ? '' : '; Anda juga tidak memegang hak kelola kredensial'}.</dd></div>
          <div className="props-row"><dt>Mapping</dt><dd>Belum tersedia — {c.provider === 'jira' ? 'project dan issue type' : 'akun/kanal tujuan'} menunggu kode terverifikasi dari kantor.</dd></div>
          <div className="props-row"><dt>Daftar tujuan</dt><dd>{c.directoryCount} entri uji</dd></div>
          <div className="props-row"><dt>Hasil uji terakhir</dt><dd>{c.lastTestNote ?? 'Belum pernah diuji.'}{c.testedAt ? ` (${instantLabel(c.testedAt, workspace.timezone)})` : ''}</dd></div>
        </dl>
        <section aria-labelledby={`log-${c.id}`}>
          <h3 id={`log-${c.id}`}>Log konfigurasi terbaru</h3>
          {c.recent.length === 0 ? <p className="tiny muted">Belum ada perubahan tercatat.</p> : (
            <ol className="plain history">
              {c.recent.map((r, i) => (
                <li key={`${r.occurredAt}-${i}`} className="activity">
                  <strong>{ACTION[r.action] ?? r.action}</strong>
                  <span className="tiny muted"> · {instantLabel(r.occurredAt, workspace.timezone)}{r.actor ? ` · oleh ${r.actor}` : ''} · {OUTCOME[r.outcome] ?? r.outcome}</span>
                </li>
              ))}
            </ol>
          )}
        </section>
      </Dialog>
      {confirmOff && (
        <ConfirmDialog title={`Nonaktifkan ${label}?`} confirmLabel="Nonaktifkan"
          impact={`Pengiriman ${label} yang masih antre di ${workspace.code} akan gagal dengan alasan "koneksi tidak aktif" dan tidak dikirim. Tiket tidak berubah; aktifkan lagi lalu coba ulang dari detail tiket.`}
          onClose={() => setConfirmOff(false)} onConfirm={() => step('disable')} />
      )}
    </>
  );
}
