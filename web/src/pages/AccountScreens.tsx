import { useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react';
import logoUrl from '../assets/logo-qash.svg';
import { ApiError, loginErrorText, rateLimitText, type Api } from '../api/client';
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
import { isTotp, loginMfaError, normalizeTotp } from '../app/mfa';
import { AccountSecurityDialog } from './MfaScreens';

/** A message carried to the login screen: why the person is back here. */
export interface LoginNotice { title: string; text: string }

/** Split layout of the owner reference (design/reference-owner-v1/01-login.png). */
export function AuthLayout({ children }: { children: ReactNode }) {
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
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  // US-1.2 second step: the challenge from a 401 mfa_required, kept only in memory.
  const [challenge, setChallenge] = useState<string | null>(null);
  const [useRecovery, setUseRecovery] = useState(false);
  const [code, setCode] = useState('');
  const [stepNotice, setStepNotice] = useState<string | null>(null);

  async function login(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError('');
    setStepNotice(null);
    try {
      await api.login(username, password);
      setPassword('');
      onSuccess(username);
    } catch (err) {
      setPassword('');
      if (err instanceof ApiError && err.code === 'mfa_required' && err.challengeId) {
        setChallenge(err.challengeId); setUseRecovery(false); setCode('');
        return;
      }
      setError(loginErrorText(err));
    } finally {
      setBusy(false);
    }
  }

  async function verify(e: FormEvent) {
    e.preventDefault();
    if (busy || !challenge) return;
    const value = code.trim();
    if (!useRecovery && !isTotp(value)) { setError('Masukkan 6 digit kode dari aplikasi autentikator.'); return; }
    if (useRecovery && !value) { setError('Masukkan recovery code.'); return; }
    setBusy(true);
    setError('');
    try {
      await api.loginMfa(challenge, useRecovery ? { recoveryCode: value } : { code: normalizeTotp(value) });
      setCode(''); setChallenge(null);
      onSuccess(username);
    } catch (err) {
      setCode('');
      const r = loginMfaError(err);
      if (r.restart) { setChallenge(null); setStepNotice(r.text); } else setError(r.text);
    } finally {
      setBusy(false);
    }
  }

  const back = () => { setChallenge(null); setCode(''); setError(''); };

  if (challenge) {
    return (
      <AuthLayout>
        <h1>Verifikasi dua langkah</h1>
        <p className="muted">{useRecovery
          ? 'Masukkan salah satu recovery code yang Anda simpan saat mengaktifkan MFA. Tiap kode hanya berlaku sekali.'
          : 'Buka aplikasi autentikator lalu masukkan kode 6 digit untuk akun ini.'}</p>
        <form className="auth-form" onSubmit={(e) => void verify(e)} noValidate>
          {useRecovery ? (
            <TextField key="recovery" label="Recovery code" name="recoveryCode" autoComplete="off" autoCapitalize="characters"
              spellCheck={false} maxLength={64} value={code} onChange={(e) => setCode(e.target.value)} disabled={busy}
              hint="Format XXXX-XXXX-XXXX-XXXX." autoFocus />
          ) : (
            <TextField key="totp" label="Kode autentikator" name="code" inputMode="numeric" autoComplete="one-time-code"
              maxLength={7} value={code} onChange={(e) => setCode(e.target.value)} disabled={busy} autoFocus />
          )}
          {error && <p role="alert" className="auth-error">{error}</p>}
          <Button type="submit" variant="primary" disabled={busy}>{busy ? 'Memverifikasi…' : 'Verifikasi'}</Button>
          <Button variant="ghost" disabled={busy} onClick={() => { setUseRecovery((v) => !v); setCode(''); setError(''); }}>
            {useRecovery ? 'Pakai kode autentikator' : 'Pakai recovery code'}
          </Button>
          <Button variant="ghost" disabled={busy} onClick={back}>Kembali</Button>
        </form>
        <p className="tiny muted auth-help">Kehilangan perangkat dan recovery code? Hubungi admin platform untuk reset MFA.</p>
      </AuthLayout>
    );
  }

  return (
    <AuthLayout>
      <h1>Masuk ke workspace</h1>
      <p className="muted">Gunakan akun internal yang diberikan administrator.</p>
      {notice && !stepNotice && (
        <div className="auth-notice" role="status">
          <strong>{notice.title}</strong>
          <span>{notice.text}</span>
        </div>
      )}
      {stepNotice && (
        <div className="auth-notice" role="status">
          <strong>Verifikasi dua langkah</strong>
          <span>{stepNotice}</span>
        </div>
      )}
      {meta.accessMode === 'password' ? (
        <form className="auth-form" onSubmit={(e) => void login(e)}>
          <TextField label="Nama pengguna" name="username" autoComplete="username" required maxLength={254}
            value={username} onChange={(e) => setUsername(e.target.value)} disabled={busy} />
          <TextField label="Kata sandi" name="password" type="password" autoComplete="current-password" required
            maxLength={1024} value={password} onChange={(e) => setPassword(e.target.value)} disabled={busy} />
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
  const [security, setSecurity] = useState(false);
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
                {session.mfaEnabled && <Button onClick={() => { close(); setSecurity(true); }}>Keamanan akun</Button>}
                <Button onClick={() => { close(); setConfirming(true); }}>Keluar dari semua perangkat</Button>
              </>
            )}
            <Button className="account-logout" onClick={() => { close(); onLogout(); }}><Icon name="logout" size={16} />Keluar</Button>
          </div>
        )}
      </Popover>
      <ChangePasswordDialog api={api} open={changing} username={username} onClose={() => setChanging(false)}
        onDone={() => { setChanging(false); onNotice(PASSWORD_CHANGED_TEXT); }} />
      <AccountSecurityDialog api={api} open={security} onClose={() => setSecurity(false)} onNotice={onNotice} />
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
