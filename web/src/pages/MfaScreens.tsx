import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from 'react';
import { ApiError, type Api } from '../api/client';
import type { MfaEnrollment, MfaStatus, Session } from '../api/types';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { TextField } from '../components/Field';
import { enrolError, groupSecret, isTotp, normalizeTotp, parseOtpauth, stepUpErrorText } from '../app/mfa';
import { AuthLayout } from './AccountScreens';

/**
 * CP4c (US-1.2, A-9) screens: authenticator enrolment, one-time recovery codes, the step-up
 * dialog and Keamanan akun. Seeds and codes live only in component state while shown; they are
 * never logged, stored or put in the URL.
 */

/** Ten recovery codes, shown once (TM-02 e). Closing means the person has stored them. */
export function RecoveryCodesDialog({ codes, onDone }: { codes: string[] | null; onDone: () => void }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    if (!codes) return;
    void navigator.clipboard?.writeText(codes.join('\n')).then(() => setCopied(true), () => setCopied(false));
  };
  return (
    <Dialog open={codes !== null} onClose={onDone} title="Simpan recovery code" initialFocus=".recovery-copy"
      footer={codes && (
        <>
          <Button className="recovery-copy" onClick={copy}>{copied ? 'Tersalin' : 'Salin semua'}</Button>
          <Button variant="primary" onClick={onDone}>Saya sudah menyimpannya</Button>
        </>
      )}>
      {codes && (
        <>
          <p>Kode ini hanya ditampilkan sekali. Simpan di tempat aman; tiap kode hanya dapat dipakai satu kali untuk masuk atau verifikasi ulang bila autentikator tidak tersedia.</p>
          <ol className="recovery-codes" aria-label="Recovery code">
            {codes.map((c) => <li key={c}><code>{c}</code></li>)}
          </ol>
          <p className="tiny muted" role="status">{copied ? 'Sepuluh kode tersalin ke clipboard.' : ''}</p>
        </>
      )}
    </Dialog>
  );
}

/**
 * Enrolment: a new seed (secret + otpauth URI; no QR library in R1), then the first code from the
 * authenticator. Success returns the ten recovery codes to the caller, which shows them once.
 */
export function MfaEnrolForm({ api, onEnrolled, onAlreadyEnrolled, actions }: {
  api: Api;
  onEnrolled: (recoveryCodes: string[]) => void;
  onAlreadyEnrolled: () => void;
  actions?: (busy: boolean) => ReactNode;
}) {
  const [enrollment, setEnrollment] = useState<MfaEnrollment | null>(null);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fieldError, setFieldError] = useState<string | undefined>();
  const [copied, setCopied] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const parts = enrollment ? parseOtpauth(enrollment.otpauthUri) : null;

  useEffect(() => {
    if (!busy && fieldError) formRef.current?.querySelector<HTMLInputElement>('input[name="code"]')?.focus();
  }, [busy, fieldError]);

  async function start() {
    setBusy(true); setError(null); setFieldError(undefined); setCode(''); setCopied(false);
    try {
      setEnrollment(await api.startMfaEnrollment());
    } catch (e) {
      const r = enrolError(e);
      if (r.enrolled) { onAlreadyEnrolled(); return; }
      setError(r.text);
    } finally {
      setBusy(false);
    }
  }

  async function confirm(e: FormEvent) {
    e.preventDefault();
    if (busy || !enrollment) return;
    if (!isTotp(code)) { setFieldError('Masukkan 6 digit kode dari aplikasi autentikator.'); return; }
    setBusy(true); setError(null); setFieldError(undefined);
    try {
      const { recoveryCodes } = await api.confirmMfaEnrollment(enrollment.enrollmentId, normalizeTotp(code));
      setEnrollment(null); setCode('');
      onEnrolled(recoveryCodes);
    } catch (err) {
      setCode('');
      const r = enrolError(err);
      if (r.enrolled) { setEnrollment(null); onAlreadyEnrolled(); return; }
      if (r.restart) setEnrollment(null);
      if (err instanceof ApiError && err.status === 422) setFieldError(r.text); else setError(r.text);
    } finally {
      setBusy(false);
    }
  }

  const copyUri = () => {
    if (!enrollment) return;
    void navigator.clipboard?.writeText(enrollment.otpauthUri).then(() => setCopied(true), () => setCopied(false));
  };

  if (!enrollment) {
    return (
      <div className="auth-form mfa-enrol">
        <ol className="mfa-steps">
          <li>Pasang aplikasi autentikator (TOTP) di ponsel Anda.</li>
          <li>Buat kunci baru, lalu tambahkan ke aplikasi.</li>
          <li>Masukkan kode 6 digit dari aplikasi untuk mengaktifkan.</li>
        </ol>
        {error && <p role="alert" className="auth-error">{error}</p>}
        <Button variant="primary" onClick={() => void start()} disabled={busy}>{busy ? 'Membuat kunci…' : 'Buat kunci autentikator'}</Button>
        {actions?.(busy)}
      </div>
    );
  }
  return (
    <form ref={formRef} className="auth-form mfa-enrol" onSubmit={(e) => void confirm(e)} noValidate>
      <div className="mfa-secret">
        <p className="tiny muted">Tambahkan akun di aplikasi autentikator dengan memasukkan kunci ini secara manual (tipe: berbasis waktu, 6 digit, 30 detik).</p>
        {parts?.account && <p className="tiny"><span className="muted">Akun:</span> {parts.issuer ? `${parts.issuer} · ` : ''}{parts.account}</p>}
        <p className="mfa-secret-key" aria-label="Kunci autentikator"><code>{parts ? groupSecret(parts.secret) : '—'}</code></p>
        <details className="mfa-uri">
          <summary>Tautan otpauth (untuk aplikasi yang menerima tautan)</summary>
          <code className="mfa-uri-text">{enrollment.otpauthUri}</code>
          <Button onClick={copyUri}>{copied ? 'Tersalin' : 'Salin tautan'}</Button>
        </details>
        <p className="tiny muted">Kode QR belum tersedia. Kunci berlaku 10 menit; jangan bagikan kepada siapa pun.</p>
      </div>
      <TextField label="Kode dari aplikasi" name="code" inputMode="numeric" autoComplete="one-time-code" maxLength={7}
        pattern="[0-9 ]*" value={code} onChange={(e) => setCode(e.target.value)} disabled={busy} error={fieldError}
        hint="6 digit, berganti setiap 30 detik." />
      {error && <p role="alert" className="auth-error">{error}</p>}
      <Button type="submit" variant="primary" disabled={busy}>{busy ? 'Memverifikasi…' : 'Aktifkan MFA'}</Button>
      <Button variant="ghost" onClick={() => void start()} disabled={busy}>Buat kunci baru</Button>
      {actions?.(busy)}
    </form>
  );
}

/** TM-02 b: a role that needs MFA without an authenticator may only enrol before the workspace opens. */
export function MfaEnrolScreen({ api, session, onDone, onLogout }: {
  api: Api;
  session: Session;
  onDone: () => void;
  onLogout: () => void;
}) {
  const [codes, setCodes] = useState<string[] | null>(null);
  return (
    <AuthLayout>
      <h1>Aktifkan verifikasi dua langkah</h1>
      <p className="muted">Halo, {session.user.displayName}. Peran Anda wajib memakai aplikasi autentikator sebelum membuka workspace.</p>
      <MfaEnrolForm api={api} onEnrolled={setCodes} onAlreadyEnrolled={onDone}
        actions={(busy) => <Button variant="ghost" disabled={busy} onClick={onLogout}>Keluar</Button>} />
      <RecoveryCodesDialog codes={codes} onDone={() => { setCodes(null); onDone(); }} />
    </AuthLayout>
  );
}

/**
 * A-9 step-up: registered with the API client while the shell is open. A request refused with
 * step_up_required opens this dialog; a successful code makes the client re-send that request
 * once. Cancel leaves the action refused (nothing changed on the server).
 */
export function StepUpDialog({ api }: { api: Api }) {
  const [open, setOpen] = useState(false);
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const failures = useRef(0);
  const resolver = useRef<((ok: boolean) => void) | null>(null);
  const id = useId();

  useEffect(() => api.onStepUpRequired(() => new Promise<boolean>((resolve) => {
    resolver.current = resolve;
    failures.current = 0;
    setCode(''); setError(null); setOpen(true);
  })), [api]);
  // Unmount (logout, session end) answers a pending prompt as cancelled.
  useEffect(() => () => { resolver.current?.(false); resolver.current = null; }, []);

  const finish = (ok: boolean) => {
    setOpen(false); setCode(''); setError(null);
    const r = resolver.current;
    resolver.current = null;
    r?.(ok);
  };

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const value = code.trim();
    if (!value) { setError('Masukkan kode 6 digit atau recovery code.'); return; }
    setBusy(true); setError(null);
    try {
      await api.stepUp(isTotp(value) ? normalizeTotp(value) : value);
      finish(true);
    } catch (err) {
      setCode('');
      if (err instanceof ApiError && err.code === 'mfa_invalid') failures.current += 1;
      setError(stepUpErrorText(err, failures.current));
      if (err instanceof ApiError && err.code === 'mfa_not_enrolled') failures.current = 0;
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onClose={() => { if (!busy) finish(false); }} title="Verifikasi ulang" initialFocus="input">
      {open && (
        <form className="auth-form step-up-form" onSubmit={(e) => void submit(e)} noValidate>
          <p>Tindakan ini sensitif. Masukkan kode dari aplikasi autentikator untuk melanjutkan; verifikasi berlaku 5 menit.</p>
          <TextField label="Kode autentikator atau recovery code" name="stepUpCode" autoComplete="one-time-code"
            maxLength={64} value={code} onChange={(e) => setCode(e.target.value)} disabled={busy}
            aria-describedby={error ? `${id}-err` : undefined}
            hint="6 digit dari aplikasi, atau recovery code XXXX-XXXX-XXXX-XXXX." />
          {error && <p id={`${id}-err`} role="alert" className="auth-error">{error}</p>}
          <div className="row">
            <Button type="submit" variant="primary" disabled={busy}>{busy ? 'Memverifikasi…' : 'Verifikasi'}</Button>
            <Button disabled={busy} onClick={() => finish(false)}>Batal</Button>
          </div>
        </form>
      )}
    </Dialog>
  );
}

const securityError = (e: unknown, fallback: string) => {
  if (e instanceof ApiError) {
    if (e.code === 'step_up_required') return 'Verifikasi ulang dibatalkan atau belum berhasil. Tidak ada yang berubah.';
    if (e.code === 'mfa_required_for_role') return 'Peran Anda wajib memakai MFA, jadi autentikator tidak dapat dimatikan.';
    if (e.code === 'mfa_not_enrolled') return 'MFA belum aktif untuk akun ini.';
  }
  return stepUpErrorText(e, 0) === 'Verifikasi belum berhasil. Coba lagi.' ? fallback : stepUpErrorText(e, 0);
};

/** Account menu → Keamanan akun: own MFA state, enrolment, new recovery codes, turning MFA off. */
export function AccountSecurityDialog({ api, open, onClose, onNotice }: {
  api: Api;
  open: boolean;
  onClose: () => void;
  onNotice: (text: string) => void;
}) {
  const [status, setStatus] = useState<MfaStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [mode, setMode] = useState<'view' | 'enrol' | 'disable'>('view');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [codes, setCodes] = useState<string[] | null>(null);

  useEffect(() => {
    if (!open) { setStatus(null); setMode('view'); setError(null); setLoadError(null); return undefined; }
    const ctl = new AbortController();
    api.getMyMfa(ctl.signal).then(setStatus, (e: unknown) => {
      if (!ctl.signal.aborted) setLoadError(securityError(e, 'Status MFA belum dapat dimuat.'));
    });
    return () => ctl.abort();
  }, [api, open, attempt]);

  const reload = () => { setStatus(null); setLoadError(null); setAttempt((n) => n + 1); };

  async function regenerate() {
    setBusy(true); setError(null);
    try {
      const { recoveryCodes } = await api.regenerateRecoveryCodes();
      setCodes(recoveryCodes);
      reload();
    } catch (e) {
      setError(securityError(e, 'Recovery code baru belum dapat dibuat. Tidak ada yang berubah.'));
    } finally {
      setBusy(false);
    }
  }

  async function disable() {
    setBusy(true); setError(null);
    try {
      await api.deleteMyMfa();
      setMode('view');
      onNotice('MFA dimatikan. Recovery code lama tidak berlaku lagi.');
      reload();
    } catch (e) {
      setError(securityError(e, 'MFA belum dapat dimatikan. Tidak ada yang berubah.'));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Dialog open={open && codes === null} onClose={() => { if (!busy) onClose(); }} title="Keamanan akun" initialFocus=".dialog-head h2">
        {open && (
          <div className="account-security">
            {loadError ? (
              <>
                <p role="alert" className="auth-error">{loadError}</p>
                <Button onClick={reload}>Coba lagi</Button>
              </>
            ) : !status ? <p className="muted" role="status">Memuat status MFA…</p> : mode === 'enrol' ? (
              <MfaEnrolForm api={api} onEnrolled={(c) => { setMode('view'); setCodes(c); reload(); onNotice('MFA aktif.'); }}
                onAlreadyEnrolled={() => { setMode('view'); reload(); }}
                actions={(b) => <Button variant="ghost" disabled={b} onClick={() => setMode('view')}>Batal</Button>} />
            ) : (
              <>
                <dl className="security-facts">
                  <div><dt>Verifikasi dua langkah</dt><dd><span className={`badge ${status.enrolled ? 'badge-ok' : 'badge-waiting'}`}>{status.enrolled ? 'Aktif' : 'Belum aktif'}</span></dd></div>
                  <div><dt>Wajib untuk peran Anda</dt><dd>{status.required ? 'Ya' : 'Tidak'}</dd></div>
                  {status.enrolled && <div><dt>Recovery code tersisa</dt><dd>{status.recoveryCodesLeft} dari 10</dd></div>}
                </dl>
                {error && <p role="alert" className="auth-error">{error}</p>}
                {!status.enrolled ? (
                  <Button variant="primary" onClick={() => setMode('enrol')}>Aktifkan MFA</Button>
                ) : mode === 'disable' ? (
                  <div className="security-confirm" role="group" aria-label="Matikan MFA">
                    <p>Matikan MFA? Masuk berikutnya cukup dengan kata sandi, dan semua recovery code tidak berlaku.</p>
                    <div className="row">
                      <Button variant="primary" onClick={() => void disable()} disabled={busy}>{busy ? 'Memproses…' : 'Matikan MFA'}</Button>
                      <Button onClick={() => setMode('view')} disabled={busy}>Batal</Button>
                    </div>
                  </div>
                ) : (
                  <div className="row">
                    <Button onClick={() => void regenerate()} disabled={busy}>{busy ? 'Memproses…' : 'Buat recovery code baru'}</Button>
                    {!status.required && <Button variant="ghost" onClick={() => { setError(null); setMode('disable'); }} disabled={busy}>Matikan MFA</Button>}
                  </div>
                )}
                {status.enrolled && status.required && <p className="tiny muted">MFA wajib untuk peran Anda dan tidak dapat dimatikan sendiri. Bila perangkat hilang, minta admin platform me-reset MFA.</p>}
                <p className="tiny muted">Membuat recovery code baru dan mematikan MFA memerlukan verifikasi ulang.</p>
              </>
            )}
          </div>
        )}
      </Dialog>
      <RecoveryCodesDialog codes={codes} onDone={() => setCodes(null)} />
    </>
  );
}
