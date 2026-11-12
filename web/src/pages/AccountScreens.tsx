import { useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react';
import logoUrl from '../assets/logo-qash.svg';
import { ApiError, MfaChallenge, loginErrorText, rateLimitText, type Api } from '../api/client';
import type { Meta, Session } from '../api/types';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { TextField } from '../components/Field';
import { Icon } from '../components/Icon';
import { Popover } from '../components/Popover';
import { initials } from '../app/labels';
import {
  MIN_PASSWORD_LENGTH,
  PASSWORD_CHANGED_TEXT,
  passwordChangeError,
  validatePasswordChange,
  type PasswordErrors,
  type PasswordField,
} from '../app/account';

/** A message carried to the login screen: why the person is back here. */
export interface LoginNotice { title: string; text: string }

/** Split layout of the owner reference (design/reference-owner-v1/01-login.png). */
function AuthLayout({ children }: { children: ReactNode }) {
  return (
    <div className="auth-screen">
      <section className="auth-hero" aria-label="Tentang aplikasi">
        <img className="auth-hero-logo" src={logoUrl} alt="" />
        <p className="auth-eyebrow">DECA · Customer Support</p>
        <p className="auth-headline">Satu tempat untuk setiap pengaduan merchant.</p>
        <p className="auth-lead">Tiket tercatat, penanganan terlacak, dan jejak audit tersimpan. Kanal masuk dan laporan operasional dalam satu alur kerja.</p>
        <ol className="auth-points">
          <li>Tiket bernomor dengan siklus resolve &amp; reopen</li>
          <li>Peran CS, Lead CS, dan grant terpisah</li>
          <li>Jejak audit append-only untuk setiap perubahan</li>
        </ol>
        <p className="auth-foot">Akses terbatas · aktivitas tercatat</p>
      </section>
      <main className="auth-main" id="main">
        <div className="auth-card">
          <div className="auth-brand">
            <img className="brand-logo" src={logoUrl} alt="Qash" />
            <span><strong>SERVICE DESK</strong><small>ITSM · akses internal</small></span>
          </div>
          {children}
        </div>
      </main>
    </div>
  );
}

/** US-1.1 login (password mode). Every refusal reads the same; the password is cleared after each try. */
export function LoginScreen({ api, meta, notice, onSuccess }: {
  api: Api;
  meta: Meta;
  notice: LoginNotice | null;
  onSuccess: (username: string) => void;
}) {
  const [challenge, setChallenge] = useState<string | null>(null);
  const [factor, setFactor] = useState('');
  const [recovery, setRecovery] = useState(false);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  async function login(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      if (challenge) await api.loginMfa(challenge, factor.trim(), recovery);
      else await api.login(username, password);
      setPassword('');
      onSuccess(username);
    } catch (err) {
      setPassword('');
      setFactor('');
      if (err instanceof MfaChallenge) setChallenge(err.challengeId);
      else setError(challenge ? 'Kode tidak valid atau verifikasi kedaluwarsa. Coba kode baru atau kembali ke login.' : loginErrorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
      <h1>Masuk ke workspace</h1>
      <p className="muted">Gunakan akun internal yang diberikan administrator.</p>
      {notice && (
        <div className="auth-notice" role="status">
          <strong>{notice.title}</strong>
          <span>{notice.text}</span>
        </div>
      )}
      {meta.accessMode === 'password' ? (
        <form className="auth-form" onSubmit={(e) => void login(e)}>
          {!challenge && <><TextField label="Nama pengguna" name="username" autoComplete="username" required maxLength={254}
            value={username} onChange={(e) => setUsername(e.target.value)} disabled={busy} />
          <TextField label="Kata sandi" name="password" type="password" autoComplete="current-password" required
            maxLength={1024} value={password} onChange={(e) => setPassword(e.target.value)} disabled={busy} /></>}
          {challenge && <><p>Verifikasi dua langkah diperlukan untuk akun ini.</p>
            <TextField label={recovery ? 'Kode pemulihan' : 'Kode authenticator'} name="factor" autoComplete="one-time-code" required
              maxLength={64} value={factor} onChange={e => setFactor(e.target.value)} disabled={busy} />
            <Button type="button" disabled={busy} onClick={() => { setRecovery(!recovery); setFactor(''); setError(''); }}>{recovery ? 'Gunakan authenticator' : 'Gunakan kode pemulihan'}</Button>
            <Button type="button" disabled={busy} onClick={() => { setChallenge(null); setFactor(''); setError(''); }}>Kembali ke login</Button></>}
          {error && <p role="alert" className="auth-error">{error}</p>}
          <Button type="submit" variant="primary" disabled={busy}>{busy ? 'Memverifikasi…' : 'Masuk'}</Button>
        </form>
      ) : <p>Akses SSO belum dikonfigurasi.</p>}
      <p className="tiny muted auth-help">Belum punya akses? Hubungi administrator untuk pembuatan akun.</p>
      <p className="tiny muted">Akses sesuai perusahaan dan peran.</p>
    </AuthLayout>
  );
}

/**
 * The CP3a own-password form (US-1.4), shared by the forced screen and the voluntary dialog.
 * Success keeps this session (the server ends the others); every field is cleared after.
 */
function PasswordChangeForm({ api, username, onChanged, actions }: {
  api: Api;
  username: string | null;
  onChanged: () => void;
  actions: (busy: boolean) => ReactNode;
}) {
  const [currentPassword, setCurrent] = useState('');
  const [newPassword, setNew] = useState('');
  const [confirmPassword, setConfirm] = useState('');
  const [errors, setErrors] = useState<PasswordErrors>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The first refused field. Focus moves once the inputs are enabled again, so a field refused
  // by the server (answered while the form was busy and disabled) still receives it.
  const [focusField, setFocusField] = useState<PasswordField | null>(null);
  const formRef = useRef<HTMLFormElement>(null);

  useEffect(() => {
    if (busy || !focusField) return;
    formRef.current?.querySelector<HTMLInputElement>(`input[name="${focusField}"]`)?.focus();
    setFocusField(null);
  }, [busy, focusField]);

  const firstRefused = (found: PasswordErrors) =>
    (['currentPassword', 'newPassword', 'confirmPassword'] as const).find((f) => found[f]) ?? null;

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    setFormError(null);
    const found = validatePasswordChange({ currentPassword, newPassword, confirmPassword, username });
    setErrors(found);
    if (Object.keys(found).length > 0) { setFocusField(firstRefused(found)); return; }
    setBusy(true);
    try {
      await api.changePassword(currentPassword, newPassword);
      setCurrent(''); setNew(''); setConfirm('');
      onChanged();
    } catch (err) {
      const { fields, form } = passwordChangeError(err);
      if (fields.currentPassword) setCurrent('');
      setErrors(fields);
      setFormError(form);
      setFocusField(firstRefused(fields));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form ref={formRef} className="auth-form" onSubmit={(e) => void submit(e)} noValidate>
      <TextField label="Kata sandi saat ini" name="currentPassword" type="password" autoComplete="current-password"
        maxLength={1024} value={currentPassword} onChange={(e) => setCurrent(e.target.value)} disabled={busy}
        error={errors.currentPassword} />
      <TextField label="Kata sandi baru" name="newPassword" type="password" autoComplete="new-password"
        maxLength={1024} value={newPassword} onChange={(e) => setNew(e.target.value)} disabled={busy}
        hint={`Minimal ${MIN_PASSWORD_LENGTH} karakter, tidak sama dengan nama pengguna atau kata sandi saat ini.`}
        error={errors.newPassword} />
      <TextField label="Ulangi kata sandi baru" name="confirmPassword" type="password" autoComplete="new-password"
        maxLength={1024} value={confirmPassword} onChange={(e) => setConfirm(e.target.value)} disabled={busy}
        error={errors.confirmPassword} />
      {formError && <p role="alert" className="auth-error">{formError}</p>}
      {actions(busy)}
    </form>
  );
}

/**
 * US-1.4 forced change: the only screen while `mustChangePassword` is set (the server refuses the
 * rest with 403). Success keeps this session; the app then loads the workspace as usual.
 */
export function ChangePasswordScreen({ api, session, username, onChanged, onLogout }: {
  api: Api;
  session: Session;
  username: string | null;
  onChanged: () => void;
  onLogout: () => void;
}) {
  return (
    <AuthLayout>
      <h1>Ganti kata sandi</h1>
      <p className="muted">Halo, {session.user.displayName}. Kata sandi Anda bersifat sementara. Buat kata sandi baru sebelum membuka workspace.</p>
      <PasswordChangeForm api={api} username={username} onChanged={onChanged} actions={(busy) => (
        <>
          <Button type="submit" variant="primary" disabled={busy}>{busy ? 'Menyimpan…' : 'Simpan kata sandi baru'}</Button>
          <Button variant="ghost" disabled={busy} onClick={onLogout}>Keluar</Button>
        </>
      )} />
      <p className="tiny muted">Sesi lain milik akun ini akan diakhiri setelah kata sandi diganti.</p>
    </AuthLayout>
  );
}

/** CP3b voluntary change from the account menu: same form; this session stays signed in. */
export function ChangePasswordDialog({ api, open, username, onClose, onDone }: {
  api: Api;
  open: boolean;
  username: string | null;
  onClose: () => void;
  onDone: () => void;
}) {
  return (
    <Dialog open={open} onClose={onClose} title="Ganti kata sandi" initialFocus="input">
      {open && (
        <>
          <p className="tiny muted">Anda tetap masuk di perangkat ini. Sesi lain milik akun ini diakhiri setelah kata sandi diganti.</p>
          <PasswordChangeForm api={api} username={username} onChanged={onDone} actions={(busy) => (
            <div className="row">
              <Button type="submit" variant="primary" disabled={busy}>{busy ? 'Menyimpan…' : 'Simpan kata sandi baru'}</Button>
              <Button disabled={busy} onClick={onClose}>Batal</Button>
            </div>
          )} />
        </>
      )}
    </Dialog>
  );
}

/**
 * Avatar menu in the top bar: who is signed in and, in password mode, the voluntary password
 * change (CP3b) and US-1.6 logout everywhere.
 */
/**
 * CP-UI1 account menu behind the topbar avatar: who is signed in, own-password actions (password
 * mode), and Keluar.
 */
export function AccountMenu({ api, session, role, passwordMode, username, onLoggedOutAll, onLogout, onNotice }: {
  api: Api;
  session: Session;
  /** Same role line as the sidebar user card (one source); falls back to the account-level role. */
  role?: string;
  passwordMode: boolean;
  /** Username typed at login in this tab (client-side "not the username" check), else null. */
  username: string | null;
  onLoggedOutAll: () => void;
  /** Ends this session (the shell asks first when an intake draft would be lost). */
  onLogout: () => void;
  onNotice: (text: string) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [changing, setChanging] = useState(false);
  return (
    <>
      <Popover className="account-menu" label="Menu akun"
        trigger={<><span className="sr-only">Menu akun</span><span className="avatar top-avatar" aria-hidden="true">{initials(session.user.displayName)}</span></>}>
        {(close) => (
          <div className="account-menu-body">
            <div className="account-id">
              <span className="avatar top-avatar" aria-hidden="true">{initials(session.user.displayName)}</span>
              <div>
                <span className="sr-only">Akun: </span><strong>{session.user.displayName}</strong>
                <p className="tiny muted">{role ?? accountRole(session)}</p>
              </div>
            </div>
            {passwordMode && (
              <>
                <Button onClick={() => { close(); setChanging(true); }}>Ganti kata sandi</Button>
                <Button onClick={() => { close(); setConfirming(true); }}>Keluar dari semua perangkat</Button>
              </>
            )}
            <Button className="account-logout" onClick={() => { close(); onLogout(); }}><Icon name="logout" size={16} />Keluar</Button>
          </div>
        )}
      </Popover>
      <ChangePasswordDialog api={api} open={changing} username={username} onClose={() => setChanging(false)}
        onDone={() => { setChanging(false); onNotice(PASSWORD_CHANGED_TEXT); }} />
      <LogoutAllDialog api={api} open={confirming} onClose={() => setConfirming(false)} onDone={onLoggedOutAll} />
    </>
  );
}

/** Role line under the name (account menu and sidebar user card). */
export function accountRole(session: Session): string {
  return session.uatSuperadmin ? 'Superadmin' : session.platformAdmin ? 'Admin platform' : 'Pengguna';
}

export function LogoutAllDialog({ api, open, onClose, onDone }: {
  api: Api;
  open: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = () => { if (!busy) { setError(null); onClose(); } };

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      await api.logoutAll();
      onDone();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return; // session already ended: the app shows the login
      setError(e instanceof ApiError && e.status === 429 ? rateLimitText(e)
        : 'Belum dapat keluar dari semua perangkat. Coba lagi.');
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onClose={close} title="Keluar dari semua perangkat?" initialFocus=".logout-all-cancel"
      footer={open && (
        <>
          <Button className="logout-all-cancel" onClick={close} disabled={busy}>Batal</Button>
          <Button variant="primary" onClick={() => void confirm()} disabled={busy}>
            {busy ? 'Memproses…' : 'Keluar dari semua perangkat'}
          </Button>
        </>
      )}>
      <p>Semua sesi akun ini diakhiri, termasuk sesi di perangkat ini. Anda perlu masuk kembali.</p>
      <p className="tiny muted">Draf tiket yang belum terkirim di tab ini akan hilang.</p>
      {error && <p role="alert" className="auth-error">{error}</p>}
    </Dialog>
  );
}


/** MFA gates boot before protected workspace requests; secrets remain in component memory. */
export function MfaSetupScreen({ api, reauthenticate, onDone }: { api: Api; reauthenticate: boolean; onDone: () => void }) {
  const [enrollment, setEnrollment] = useState<{ enrollmentId: string; otpauthUri: string; expiresAt: string } | null>(null);
  const [codes, setCodes] = useState<string[] | null>(null);
  const [saved, setSaved] = useState(false);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function act(work: () => Promise<void>) {
    if (busy) return;
    setBusy(true); setError('');
    try { await work(); } catch (e) {
      setError(e instanceof ApiError && e.status === 429 ? rateLimitText(e) : 'Verifikasi belum berhasil. Periksa kode dan waktu di authenticator, lalu coba lagi.');
    } finally { setBusy(false); }
  }
  const secret = enrollment ? new URL(enrollment.otpauthUri).searchParams.get('secret') : '';
  return <AuthLayout><h1>{reauthenticate ? 'Verifikasi ulang akun' : 'Aktifkan verifikasi dua langkah'}</h1>
    <p className="muted">Akun administrator memerlukan authenticator sebelum mengakses perusahaan dan tiket.</p>
    {reauthenticate ? <p>Sesi ini dibuat sebelum verifikasi MFA. Silakan keluar, lalu masuk kembali menggunakan kata sandi dan kode authenticator.</p> :
      codes ? <><h2>Simpan kode pemulihan</h2><p>Kode berikut hanya ditampilkan sekali. Simpan di tempat aman; masing-masing dapat digunakan satu kali.</p>
        <pre style={{whiteSpace:'pre-wrap',userSelect:'all'}}>{codes.join('\n')}</pre>
        <label><input type="checkbox" checked={saved} onChange={e => setSaved(e.target.checked)} /> Saya sudah menyimpan kode pemulihan</label>
        <Button variant="primary" disabled={!saved} onClick={() => { setCodes(null); onDone(); }}>Lanjut ke aplikasi</Button></> :
      enrollment ? <form className="auth-form" onSubmit={e => { e.preventDefault(); void act(async () => {
        const result = await api.confirmMfaEnrollment(enrollment.enrollmentId, code.trim());
        setCode(''); setEnrollment(null); setCodes(result.recoveryCodes);
      }); }}><p>Tambahkan akun di aplikasi authenticator dengan kunci berikut (TOTP, 6 digit, interval 30 detik).</p>
        <code style={{overflowWrap:'anywhere',userSelect:'all'}}>{secret}</code>
        <p className="tiny muted">Jangan membagikan kunci ini. Jika kedaluwarsa, buat kunci baru.</p>
        <TextField label="Kode authenticator" name="code" autoComplete="one-time-code" inputMode="numeric" pattern="[0-9]{6}" required maxLength={6} value={code} onChange={e => setCode(e.target.value)} disabled={busy} />
        <Button type="submit" variant="primary" disabled={busy}>Verifikasi dan aktifkan</Button>
        <Button type="button" disabled={busy} onClick={() => void act(async () => { setEnrollment(await api.startMfaEnrollment()); setCode(''); })}>Buat kunci baru</Button>
      </form> : <Button variant="primary" disabled={busy} onClick={() => void act(async () => setEnrollment(await api.startMfaEnrollment()))}>Siapkan authenticator</Button>}
    {error && <p role="alert" className="auth-error">{error}</p>}
    {!codes && <Button disabled={busy} onClick={() => void act(async () => { await api.logout(); onDone(); })}>Keluar</Button>}
  </AuthLayout>;
}
