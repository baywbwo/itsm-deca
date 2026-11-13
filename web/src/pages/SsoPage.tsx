import { useId, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type { SsoCompany, SsoOverview, SsoProvider, SsoIdentity, SsoRoleMapping, UserListItem } from '../api/types';
import { instantLabel } from '../app/format';
import {
  SSO_FIELD_TEXT, emptySsoDraft, ssoBody, ssoDraftOf, ssoEnableBlocker, ssoMoves, ssoState, ssoTestText,
  type SsoDraft, type SsoState,
} from '../app/sso';
import { Badge, type BadgeTone } from '../components/Badge';
import { Button } from '../components/Button';
import { DataTable } from '../components/DataTable';
import { Dialog } from '../components/Dialog';
import { SelectField, TextField } from '../components/Field';
import { PageHeader } from '../components/PageHeader';
import { ErrorState, ForbiddenState, LoadingState } from '../components/States';
import { ConfirmDialog, ROLE_LABEL, useLoad, WriteAlert, writeError, type WriteError } from './adminKit';

const STATE: Record<SsoState, { label: string; tone: BadgeTone }> = {
  none: { label: 'Belum diatur', tone: 'neutral' },
  draft: { label: 'Belum siap', tone: 'waiting' },
  ready: { label: 'Siap diaktifkan', tone: 'blue' },
  on: { label: 'Aktif', tone: 'ok' },
};

const AUTH_LABEL: Record<SsoOverview['server']['auth'], string> = {
  password: 'Kata sandi saja (SSO mati di server)',
  sso: 'SSO saja',
  both: 'Kata sandi dan SSO',
};

/** SSO admin refusal → what the admin reads; the generic 409 text would mislead for a link conflict. */
export function ssoWriteError(e: unknown, what: string): WriteError {
  if (e instanceof ApiError) {
    const rid = e.requestId ? ` (ID permintaan ${e.requestId})` : '';
    const own = (title: string, body: string, conflict = false): WriteError => ({ title, body: `${body}${rid}`, conflict, field: null });
    switch (e.code) {
      case 'keyring_missing':
        return own('Kunci enkripsi belum dipasang', 'Server belum memiliki ITSM_KEYRING; client secret tidak dapat disimpan dan SSO tidak dapat diaktifkan.');
      case 'idp_not_ready':
        return own('Belum siap diaktifkan', 'Simpan client secret dan uji koneksi sampai berhasil sebelum mengaktifkan.');
      case 'subject_linked':
        return own('Subject sudah terhubung', 'Subject ini sudah terhubung ke akun lain di penyedia ini. Putuskan dulu bila memang perlu dipindah.');
      case 'user_linked':
        return own('Akun sudah terhubung', 'Akun ini sudah punya identitas SSO di penyedia ini. Putuskan yang lama dahulu.');
      case 'user_in_other_company':
        return own('Akun milik perusahaan lain', 'Akun ini anggota perusahaan lain; satu akun hanya untuk satu perusahaan.');
      case 'user_inactive':
        return own('Akun nonaktif', 'Akun nonaktif tidak dapat dihubungkan.');
      case 'rule_violation':
        return own('Tidak ada perubahan', `${e.message} Muat ulang data bila status di layar berbeda.`, true);
      default:
        break;
    }
    if (e.status === 422 && e.field && SSO_FIELD_TEXT[e.field]) {
      return { title: 'Isian tidak valid', body: `${SSO_FIELD_TEXT[e.field]} Tidak ada yang berubah.${rid}`, conflict: false, field: e.field };
    }
    if (e.status === 403) return own('Tidak diizinkan', 'Hanya admin platform yang dapat mengatur SSO. Tidak ada yang berubah.');
  }
  return writeError(e, what);
}

/**
 * CP5c Login SSO (platform admin, password mode): one OpenID Connect provider per company. The client
 * secret is write-only; a provider is switched on only after a stored secret and a successful test,
 * and changing where it points (issuer, client id, hosts) switches it off until it is tested again.
 */
export function SsoPage({ api, timezone, onNotice }: { api: Api; timezone: string; onNotice: (t: string) => void }) {
  const res = useLoad((s) => api.getPlatformSso(s), 'sso');
  const [open, setOpen] = useState<string | null>(null);
  const header = (
    <PageHeader title="Login SSO"
      description="Penyedia login kantor (OpenID Connect) per perusahaan · hanya admin platform" />
  );
  if (res.error) {
    if (res.error instanceof ApiError && (res.error.status === 403 || res.error.status === 404)) return <>{header}<ForbiddenState /></>;
    return (
      <>
        {header}
        <ErrorState title="Pengaturan SSO belum dapat dimuat" body={res.error instanceof ApiError ? res.error.message : 'Terjadi kesalahan.'}
          requestId={res.error instanceof ApiError ? res.error.requestId : null} onRetry={res.reload} />
      </>
    );
  }
  const data = res.data;
  if (!data) return <>{header}<LoadingState label="Memuat pengaturan SSO…" /></>;
  const { server } = data;
  const patch = (companyId: string, f: (c: SsoCompany) => SsoCompany) =>
    res.set({ ...data, items: data.items.map((c) => (c.companyId === companyId ? f(c) : c)) });
  const target = data.items.find((c) => c.companyId === open) ?? null;
  return (
    <>
      {header}
      {server.lab && (
        <p className="banner spaced-bottom" role="note">
          Server berjalan dengan <strong>ITSM_OIDC_LAB</strong>: issuer http lokal diizinkan untuk uji. Jangan dipakai di produksi.
        </p>
      )}
      <section className="pcard spaced-bottom" aria-labelledby="sso-server">
        <div className="hd"><h2 id="sso-server">Server</h2></div>
        <div className="bd">
          <dl className="kv">
            <dt>Mode login</dt><dd className="kv-text">{AUTH_LABEL[server.auth]}</dd>
            <dt>Redirect URI</dt>
            <dd className="kv-text">
              {server.redirectUri
                ? <><code data-testid="sso-redirect">{server.redirectUri}</code><br /><span className="tiny muted">Daftarkan alamat ini di aplikasi penyedia login.</span></>
                : 'Belum diatur (ITSM_OIDC_REDIRECT_URI); login SSO tidak dapat dimulai.'}
            </dd>
            <dt>Kunci enkripsi</dt>
            <dd className="kv-text">{server.keyring ? 'Terpasang' : 'Belum dipasang (ITSM_KEYRING): client secret tidak dapat disimpan.'}</dd>
          </dl>
        </div>
      </section>
      <DataTable caption="Perusahaan dan penyedia login" rows={data.items} rowKey={(c) => c.companyId}
        empty={{ title: 'Belum ada perusahaan', body: 'Buat perusahaan dahulu di Pengaturan.' }}
        columns={[
          { key: 'company', header: 'Perusahaan', cell: (c) => (
            <><strong>{c.code}</strong> · {c.name}{!c.active && <> <Badge tone="waiting">Nonaktif</Badge></>}</>
          ) },
          { key: 'provider', header: 'Penyedia', cell: (c) => c.provider?.displayName ?? '—' },
          { key: 'state', header: 'Status', cell: (c) => {
            const s = STATE[ssoState(c.provider, server.keyring)];
            return <Badge tone={s.tone}>{s.label}</Badge>;
          } },
          { key: 'test', header: 'Uji terakhir', cell: (c) => testLabel(c.provider, timezone) },
          { key: 'ids', header: 'Akun terhubung', cell: (c) => String(c.identities) },
          { key: 'act', header: 'Aksi', cell: (c) => (
            <Button className="btn-small" variant={c.provider ? 'secondary' : 'primary'} onClick={() => setOpen(c.companyId)}
              aria-label={`${c.provider ? 'Kelola' : 'Atur'} SSO ${c.code}`}>{c.provider ? 'Kelola' : 'Atur'}</Button>
          ) },
        ]} />
      {target && (
        <SsoDialog api={api} server={server} company={target} timezone={timezone} onClose={() => setOpen(null)}
          onReload={res.reload} onNotice={onNotice}
          onProvider={(p) => patch(target.companyId, (c) => ({ ...c, provider: p }))}
          onCount={(d) => patch(target.companyId, (c) => ({ ...c, identities: Math.max(0, c.identities + d) }))} />
      )}
    </>
  );
}

function testLabel(p: SsoProvider | null, tz: string): string {
  if (!p) return '—';
  if (p.lastTestOk === null) return 'Belum diuji';
  return `${p.lastTestOk ? 'Berhasil' : 'Gagal'}${p.lastTestedAt ? ` · ${instantLabel(p.lastTestedAt, tz)}` : ''}`;
}

/** Labelled multi-line list (domains, hosts): one entry per line. */
function ListField({ label, hint, error, value, disabled, onChange, placeholder }: {
  label: string; hint: string; error?: string | undefined; value: string; disabled: boolean; placeholder: string;
  onChange: (v: string) => void;
}) {
  const id = useId();
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      <textarea id={id} className="field-control" rows={2} value={value} disabled={disabled} placeholder={placeholder}
        spellCheck={false} autoComplete="off" maxLength={4000} aria-invalid={error ? true : undefined}
        aria-describedby={`${id}-hint${error ? ` ${id}-error` : ''}`} onChange={(e) => onChange(e.target.value)} />
      <span id={`${id}-hint`} className="field-hint">{hint}</span>
      {error && <span id={`${id}-error`} className="field-error">{error}</span>}
    </div>
  );
}

type Busy = null | 'save' | 'secret' | 'test' | 'enable';

function SsoDialog({ api, server, company: c, timezone, onClose, onReload, onNotice, onProvider, onCount }: {
  api: Api; server: SsoOverview['server']; company: SsoCompany; timezone: string;
  onClose: () => void; onReload: () => void; onNotice: (t: string) => void;
  onProvider: (p: SsoProvider) => void; onCount: (delta: number) => void;
}) {
  const p = c.provider;
  const [draft, setDraft] = useState<SsoDraft>(() => (p ? ssoDraftOf(p) : emptySsoDraft()));
  const [secret, setSecret] = useState('');
  const [busy, setBusy] = useState<Busy>(null);
  const [error, setError] = useState<WriteError | null>(null);
  const [test, setTest] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<null | 'move' | 'disable'>(null);
  const formId = useId();
  const secretId = useId();
  const disabled = busy !== null;
  const dirty = JSON.stringify(draft) !== JSON.stringify(p ? ssoDraftOf(p) : emptySsoDraft());
  const moves = !!p && dirty && ssoMoves(p, draft);
  const set = <K extends keyof SsoDraft>(k: K, v: SsoDraft[K]) => setDraft((d) => ({ ...d, [k]: v }));
  const setRow = (i: number, m: SsoRoleMapping) => set('roleMapping', draft.roleMapping.map((x, j) => (j === i ? m : x)));
  const fieldError = (f: string) => (error?.field === f ? SSO_FIELD_TEXT[f] : undefined);

  async function run<T>(kind: Busy, what: string, f: () => Promise<T>): Promise<T | null> {
    setBusy(kind);
    setError(null);
    try {
      return await f();
    } catch (e) {
      setError(ssoWriteError(e, what));
      return null;
    } finally {
      setBusy(null);
      setConfirm(null);
    }
  }

  async function save() {
    const r = await run('save', 'Pengaturan SSO', () => api.saveCompanySso(c.companyId, ssoBody(draft, p?.version ?? null)));
    if (!r) return;
    onProvider(r.item);
    setDraft(ssoDraftOf(r.item));
    setTest(null);
    onNotice(!p ? `Penyedia SSO ${c.code} dibuat (belum aktif). Simpan client secret lalu uji koneksi.`
      : p.enabled && !r.item.enabled ? `Pengaturan SSO ${c.code} tersimpan. SSO dimatikan dan sesi SSO diakhiri; uji ulang lalu aktifkan.`
        : `Pengaturan SSO ${c.code} tersimpan. Tercatat di audit.`);
  }
  async function saveSecret() {
    if (!p || !secret) return;
    const r = await run('secret', 'Client secret', () => api.setCompanySsoSecret(c.companyId, secret, p.version));
    if (!r) return;
    setSecret('');
    onProvider(r.item);
    onNotice(`Client secret ${c.code} tersimpan terenkripsi. Tercatat di audit (tanpa nilai).`);
  }
  async function runTest() {
    if (!p) return;
    const r = await run('test', 'Uji koneksi', () => api.testCompanySso(c.companyId, p.version));
    if (!r) return;
    onProvider(r.item);
    setTest(ssoTestText(r.result));
  }
  async function setEnabled(enabled: boolean) {
    if (!p) return;
    const r = await run('enable', 'Status SSO', () => api.setCompanySsoEnabled(c.companyId, enabled, p.version));
    if (!r) return;
    onProvider(r.item);
    onNotice(enabled ? `SSO ${c.code} aktif. Pengguna dapat masuk dengan kode perusahaan ${c.code}.`
      : `SSO ${c.code} dimatikan; sesi yang masuk lewat SSO diakhiri.`);
  }

  const blocker = p ? ssoEnableBlocker(p, server.keyring) : null;
  return (
    <>
      <Dialog open onClose={disabled ? () => undefined : onClose} title={`Login SSO · ${c.code}`} variant="peek" initialFocus="input"
        footer={(
          <>
            <Button onClick={onClose} disabled={disabled}>Tutup</Button>
            {p && (
              <Button disabled={disabled || dirty} aria-busy={busy === 'test'} onClick={() => void runTest()}
                title={dirty ? 'Simpan pengaturan dahulu' : undefined}>{busy === 'test' ? 'Menguji…' : 'Uji koneksi'}</Button>
            )}
            {p?.enabled && <Button disabled={disabled} onClick={() => setConfirm('disable')}>Matikan SSO</Button>}
            {p && !p.enabled && (
              <Button variant="primary" disabled={disabled || dirty || !!blocker} title={blocker ?? undefined}
                aria-busy={busy === 'enable'} onClick={() => void setEnabled(true)}>Aktifkan SSO</Button>
            )}
          </>
        )}>
        <p className="banner spaced-bottom">
          {c.code} · {c.name} · <strong>{STATE[ssoState(p, server.keyring)].label}</strong>
          {p && !p.enabled && blocker && <> · {blocker}</>}
        </p>
        {error && <WriteAlert error={error} onReload={onReload} />}
        {test && <p className="hintline" role="status" data-testid="sso-test">{test}</p>}

        <section className="int-section" aria-labelledby={`${formId}-h`}>
          <h3 id={`${formId}-h`}>Penyedia login</h3>
          <form id={formId} className="int-form" autoComplete="off" onSubmit={(e) => {
            e.preventDefault();
            if (disabled || !dirty) return;
            if (moves && p?.enabled) setConfirm('move'); else void save();
          }}>
            <TextField label="Nama tampilan" required maxLength={80} value={draft.displayName} disabled={disabled}
              placeholder="Microsoft Entra ID kantor" error={fieldError('displayName')} onChange={(e) => set('displayName', e.target.value)} />
            <TextField label="Issuer" required type="url" inputMode="url" maxLength={512} value={draft.issuer} disabled={disabled}
              placeholder="https://login.microsoftonline.com/<tenant>/v2.0" spellCheck={false}
              hint="Alamat issuer OpenID Connect; dokumen discovery dibaca dari /.well-known/openid-configuration."
              error={fieldError('issuer')} onChange={(e) => set('issuer', e.target.value)} />
            <TextField label="Client ID" required maxLength={255} value={draft.clientId} disabled={disabled} spellCheck={false}
              error={fieldError('clientId')} onChange={(e) => set('clientId', e.target.value)} />
            <ListField label="Domain email untuk akun baru" value={draft.emailDomains} disabled={disabled} placeholder="kantor.co.id"
              hint="Satu per baris. Hanya email terverifikasi dari domain ini yang dibuatkan akun otomatis; kosong = semua akun harus dihubungkan admin."
              error={fieldError('emailDomains')} onChange={(v) => set('emailDomains', v)} />
            <ListField label="Host tambahan (opsional)" value={draft.extraHosts} disabled={disabled} placeholder="oauth2.googleapis.com"
              hint="Satu per baris. Host endpoint token/JWKS di luar host issuer (mis. Google)."
              error={fieldError('extraHosts')} onChange={(v) => set('extraHosts', v)} />
            <TextField label="Klaim grup" required maxLength={100} value={draft.groupClaim} disabled={disabled} spellCheck={false}
              hint="Nama klaim di ID token yang berisi grup (biasanya groups)." error={fieldError('groupClaim')}
              onChange={(e) => set('groupClaim', e.target.value)} />
          </form>
          <fieldset className="int-map">
            <legend>Pemetaan grup ke peran</legend>
            <p className="tiny muted">
              Grup pertama yang cocok menentukan peran akun baru. Tanpa grup yang cocok, akun dibuat tanpa akses sampai admin memberi peran.
            </p>
            {draft.roleMapping.map((m, i) => (
              <div key={i} className="row">
                <TextField label={`Grup ${i + 1}`} maxLength={255} value={m.group} disabled={disabled} spellCheck={false}
                  onChange={(e) => setRow(i, { ...m, group: e.target.value })} />
                <SelectField label={`Peran grup ${i + 1}`} value={m.role} disabled={disabled}
                  onChange={(e) => setRow(i, { ...m, role: e.target.value as SsoRoleMapping['role'] })}>
                  <option value="cs">{ROLE_LABEL.cs}</option>
                  <option value="lead_cs">{ROLE_LABEL.lead_cs}</option>
                </SelectField>
                <Button className="btn-small" disabled={disabled} aria-label={`Hapus pemetaan grup ${i + 1}`}
                  onClick={() => set('roleMapping', draft.roleMapping.filter((_, j) => j !== i))}>Hapus</Button>
              </div>
            ))}
            {fieldError('roleMapping') && <p className="field-error">{fieldError('roleMapping')}</p>}
            <div className="row">
              <Button className="btn-small" disabled={disabled || draft.roleMapping.length >= 50}
                onClick={() => set('roleMapping', [...draft.roleMapping, { group: '', role: 'cs' }])}>Tambah pemetaan</Button>
            </div>
            <label className="row">
              <input type="checkbox" checked={draft.syncRoles} disabled={disabled} onChange={(e) => set('syncRoles', e.target.checked)} />
              <span>Perbarui peran dari grup di setiap login (bukan hanya saat akun dibuat)</span>
            </label>
          </fieldset>
          {moves && (
            <p className="hintline" role="status">
              Issuer, client ID atau host berubah: {p?.enabled ? 'SSO akan dimatikan, sesi SSO diakhiri, dan ' : ''}koneksi harus diuji ulang
              sebelum diaktifkan.
            </p>
          )}
          <div className="row spaced-bottom">
            <Button type="submit" form={formId} variant="primary" disabled={disabled || !dirty} aria-busy={busy === 'save'}>
              {busy === 'save' ? 'Menyimpan…' : p ? 'Simpan pengaturan' : 'Buat penyedia'}
            </Button>
          </div>
        </section>

        {p && (
          <section className="int-section" aria-labelledby={`${secretId}-h`}>
            <h3 id={`${secretId}-h`}>Client secret</h3>
            <dl className="props spaced-bottom">
              <div className="props-row">
                <dt>Status</dt>
                <dd>{p.secretSet ? `Tersimpan${p.secretUpdatedAt ? ` · diganti ${instantLabel(p.secretUpdatedAt, timezone)}` : ''}` : 'Belum disimpan'}</dd>
              </div>
            </dl>
            <form className="row spaced-bottom" autoComplete="off" onSubmit={(e) => { e.preventDefault(); if (!disabled) void saveSecret(); }}>
              <TextField label={p.secretSet ? 'Ganti client secret' : 'Client secret'} type="password" autoComplete="new-password"
                maxLength={4000} value={secret} disabled={disabled || !server.keyring} spellCheck={false}
                hint="Disimpan terenkripsi di server dan tidak pernah ditampilkan lagi." error={fieldError('secret')}
                onChange={(e) => setSecret(e.target.value)} />
              <Button type="submit" disabled={disabled || !secret || !server.keyring} aria-busy={busy === 'secret'}>
                {busy === 'secret' ? 'Menyimpan…' : p.secretSet ? 'Ganti secret' : 'Simpan secret'}
              </Button>
            </form>
            <p className="tiny muted">Hasil uji terakhir: {testLabel(p, timezone)}</p>
          </section>
        )}

        {p && <Identities api={api} company={c} timezone={timezone} onNotice={onNotice} onCount={onCount} />}
      </Dialog>
      {confirm === 'move' && (
        <ConfirmDialog title={`Ubah tujuan SSO ${c.code}?`} confirmLabel="Simpan dan matikan SSO"
          impact="Issuer, client ID atau host berubah. SSO perusahaan ini dimatikan dan semua sesi yang masuk lewat SSO diakhiri sampai koneksi diuji ulang dan diaktifkan lagi."
          onClose={() => setConfirm(null)} onConfirm={save} />
      )}
      {confirm === 'disable' && (
        <ConfirmDialog title={`Matikan SSO ${c.code}?`} confirmLabel="Matikan SSO"
          impact="Pengguna perusahaan ini tidak dapat masuk dengan SSO dan semua sesi yang masuk lewat SSO diakhiri. Akun dan hubungan identitas tetap tersimpan."
          onClose={() => setConfirm(null)} onConfirm={() => setEnabled(false)} />
      )}
    </>
  );
}

/** Accounts linked to the company's provider: search, link by subject, unlink (ends that SSO session). */
function Identities({ api, company: c, timezone, onNotice, onCount }: {
  api: Api; company: SsoCompany; timezone: string; onNotice: (t: string) => void; onCount: (delta: number) => void;
}) {
  const [text, setText] = useState('');
  const [q, setQ] = useState('');
  const res = useLoad((s) => api.listCompanySsoIdentities(c.companyId, q, s), `${c.companyId}|${q}`);
  const [linking, setLinking] = useState(false);
  const [unlinking, setUnlinking] = useState<SsoIdentity | null>(null);
  const [error, setError] = useState<WriteError | null>(null);
  const headId = useId();
  const list = res.data;

  return (
    <section className="int-section" aria-labelledby={headId}>
      <h3 id={headId}>Akun terhubung{list ? ` (${list.total})` : ''}</h3>
      <div className="row spaced-bottom">
        <form className="row" role="search" onSubmit={(e) => { e.preventDefault(); setQ(text.trim()); }}>
          <TextField label="Cari nama, email atau subject" type="search" maxLength={100} value={text}
            onChange={(e) => setText(e.target.value)} />
          <Button type="submit">Cari</Button>
        </form>
        <Button variant="primary" onClick={() => { setError(null); setLinking(true); }}>Hubungkan akun</Button>
      </div>
      {error && <WriteAlert error={error} onReload={res.reload} />}
      {res.error ? (
        <ErrorState title="Akun terhubung belum dapat dimuat" body={res.error instanceof ApiError ? res.error.message : 'Terjadi kesalahan.'}
          requestId={res.error instanceof ApiError ? res.error.requestId : null} onRetry={res.reload} />
      ) : !list ? <LoadingState label="Memuat akun terhubung…" /> : (
        <>
          <DataTable caption={`Akun terhubung ke SSO ${c.code}`} rows={list.items} rowKey={(i) => i.id}
            empty={{ title: q ? 'Tidak ada yang cocok' : 'Belum ada akun terhubung',
              body: q ? 'Ubah kata kunci pencarian.' : 'Akun dibuat otomatis saat login pertama (domain email terdaftar) atau dihubungkan di sini.' }}
            columns={[
              { key: 'user', header: 'Akun', cell: (i) => (
                <><strong>{i.displayName}</strong>{i.email && <><br /><span className="tiny muted">{i.email}</span></>}
                  {!i.userActive && <> <Badge tone="waiting">Nonaktif</Badge></>}</>
              ) },
              { key: 'subject', header: 'Subject', cell: (i) => <code>{i.subject}</code> },
              { key: 'source', header: 'Asal', cell: (i) => (i.provisioned ? 'Dibuat saat login' : 'Dihubungkan admin') },
              { key: 'last', header: 'Login terakhir', cell: (i) => (i.lastLoginAt ? instantLabel(i.lastLoginAt, timezone) : 'Belum pernah') },
              { key: 'act', header: 'Aksi', cell: (i) => (
                <Button className="btn-small" aria-label={`Putuskan ${i.displayName}`} onClick={() => setUnlinking(i)}>Putuskan</Button>
              ) },
            ]} />
          {list.total > list.items.length && (
            <p className="tiny muted">Menampilkan {list.items.length} dari {list.total}. Persempit pencarian untuk melihat sisanya.</p>
          )}
        </>
      )}
      {linking && (
        <LinkDialog api={api} company={c} onClose={() => setLinking(false)} onLinked={(i) => {
          setLinking(false);
          onCount(1);
          res.reload();
          onNotice(`${i.displayName} dihubungkan ke SSO ${c.code}. Tercatat di audit.`);
        }} />
      )}
      {unlinking && (
        <ConfirmDialog title={`Putuskan ${unlinking.displayName}?`} confirmLabel="Putuskan"
          impact="Akun ini tidak dapat lagi masuk lewat SSO dengan identitas ini, dan sesi SSO-nya diakhiri. Akun ITSM dan riwayatnya tetap ada."
          onClose={() => setUnlinking(null)} onConfirm={async () => {
            const who = unlinking;
            try {
              await api.unlinkCompanySsoIdentity(c.companyId, who.id);
              onCount(-1);
              onNotice(`${who.displayName} diputuskan dari SSO ${c.code}. Tercatat di audit.`);
            } catch (e) {
              setError(ssoWriteError(e, 'Hubungan akun'));
            } finally {
              setUnlinking(null);
              res.reload();
            }
          }} />
      )}
    </section>
  );
}

/** Pick an existing active account of this company (or one without a company) and enter its IdP subject. */
function LinkDialog({ api, company: c, onClose, onLinked }: {
  api: Api; company: SsoCompany; onClose: () => void; onLinked: (i: SsoIdentity) => void;
}) {
  const [text, setText] = useState('');
  const [found, setFound] = useState<UserListItem[] | null>(null);
  const [userId, setUserId] = useState('');
  const [subject, setSubject] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const formId = useId();
  const other = (u: UserListItem) => !!u.company && u.company.workspaceId !== c.workspaceId;

  async function search() {
    setBusy(true);
    setError(null);
    try {
      const r = await api.listUsers({ q: text.trim(), active: 'true' }, null);
      setFound(r.items);
      setUserId(r.items.find((u) => !other(u))?.id ?? '');
    } catch (e) {
      setError(ssoWriteError(e, 'Pencarian pengguna'));
    } finally {
      setBusy(false);
    }
  }
  async function link() {
    setBusy(true);
    setError(null);
    try {
      const r = await api.linkCompanySsoIdentity(c.companyId, userId, subject.trim());
      onLinked(r.item);
    } catch (e) {
      setError(ssoWriteError(e, 'Hubungan akun'));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title={`Hubungkan akun ke SSO ${c.code}`} initialFocus="input"
      footer={(
        <>
          <Button onClick={onClose} disabled={busy}>Batal</Button>
          <Button type="submit" form={formId} variant="primary" disabled={busy || !userId || !subject.trim()} aria-busy={busy}>
            {busy ? 'Memproses…' : 'Hubungkan'}
          </Button>
        </>
      )}>
      <p className="tiny muted spaced-bottom">
        Dipakai bila penyedia tidak mengirim email terverifikasi (mis. Microsoft Entra) atau akun sudah ada sebelum SSO.
        Subject adalah ID unik pengguna di penyedia (klaim <code>sub</code>).
      </p>
      {error && <WriteAlert error={error} />}
      <form className="row spaced-bottom" role="search" onSubmit={(e) => { e.preventDefault(); if (!busy) void search(); }}>
        <TextField label="Cari pengguna" type="search" maxLength={100} value={text} disabled={busy}
          onChange={(e) => setText(e.target.value)} />
        <Button type="submit" disabled={busy}>Cari</Button>
      </form>
      <form id={formId} onSubmit={(e) => { e.preventDefault(); if (!busy && userId && subject.trim()) void link(); }}>
        {found && (found.length === 0 ? <p className="tiny muted">Tidak ada pengguna aktif yang cocok.</p> : (
          <SelectField label="Pengguna" value={userId} disabled={busy} required error={error?.field === 'userId' ? SSO_FIELD_TEXT.userId : undefined}
            onChange={(e) => setUserId(e.target.value)}>
            <option value="">— pilih pengguna —</option>
            {found.map((u) => (
              <option key={u.id} value={u.id} disabled={other(u)}>
                {u.displayName} · {u.username}{other(u) ? ` (perusahaan ${u.company?.code})` : ''}
              </option>
            ))}
          </SelectField>
        ))}
        <TextField label="Subject di penyedia" required maxLength={255} value={subject} disabled={busy} spellCheck={false} autoComplete="off"
          error={error?.field === 'subject' ? SSO_FIELD_TEXT.subject : undefined} onChange={(e) => setSubject(e.target.value)} />
      </form>
    </Dialog>
  );
}
