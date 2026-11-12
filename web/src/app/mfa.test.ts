import { describe, expect, it } from 'vitest';
import { ApiError, createApi, type TransportRequest, type TransportResponse } from '../api/client';
import { loadBoot } from './hooks';
import { enrolError, groupSecret, isRecoveryCode, isTotp, loginMfaError, parseOtpauth, stepUpErrorText } from './mfa';
import { userWriteError, validateMfaResetReason } from './users';

// CP4c (US-1.2, A-9): second login step, enrolment, step-up retry and the MFA admin actions.
// Fetch is mocked at the transport; every code, seed and challenge here is synthetic.

type Route = (req: TransportRequest, n: number) => TransportResponse | undefined;
const res = (status: number, body: unknown = null): TransportResponse => ({ status, body, requestId: 'rid-1' });
const err = (status: number, code: string, extra: Record<string, unknown> = {}) =>
  res(status, { error: { code, message: 'server text', ...extra } });
const user = { id: 'u-1', displayName: 'Sari Wulandari' };
const meta = { environment: 'production', accessMode: 'password', banner: null, uat: null };
const session = (over: Record<string, unknown> = {}) => ({
  user, banner: null, uatSuperadmin: false, mustChangePassword: false, platformAdmin: true,
  mfaEnabled: true, mfaEnrolled: true, mfaRequired: true, ...over,
});
const SYNTHETIC_URI = 'otpauth://totp/DECA%20ITSM:sari?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=DECA%20ITSM&algorithm=SHA1&digits=6&period=30';

function mock(route: Route) {
  const requests: TransportRequest[] = [];
  const api = createApi(async (req) => {
    requests.push(req);
    const hit = route(req, requests.filter((r) => r.path === req.path && r.method === req.method).length);
    if (!hit) throw new Error(`unexpected ${req.method} ${req.path}`);
    return hit;
  });
  return { api, requests, paths: () => requests.map((r) => `${r.method} ${r.path}`) };
}
const csrfOk = (r: TransportRequest) => (r.path === '/api/v1/session/csrf' ? res(200, { csrfToken: 'csrf-1' }) : undefined);
const signal = () => new AbortController().signal;

describe('CP4c two-step login (US-1.2)', () => {
  it('carries the challenge of a 401 mfa_required and completes with a TOTP code without CSRF', async () => {
    let ended = 0;
    const { api, requests } = mock((r) => {
      if (r.path === '/api/v1/login') return err(401, 'mfa_required', { challengeId: 'ch-1' });
      if (r.path === '/api/v1/login/mfa') return res(200, { ...session(), csrfToken: 'csrf-2' });
      return undefined;
    });
    api.onSessionEnded(() => { ended += 1; });
    const e = await api.login('sari', 'synthetic-pass').catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect((e as ApiError).code).toBe('mfa_required');
    expect((e as ApiError).challengeId).toBe('ch-1');
    const s = await api.loginMfa('ch-1', { code: '123456' });
    expect(s.mfaEnrolled).toBe(true);
    const step = requests.find((r) => r.path === '/api/v1/login/mfa');
    expect(step?.body).toEqual({ challengeId: 'ch-1', code: '123456' });
    expect(step?.headers['X-ITSM-Login']).toBe('1');
    expect(step?.headers['X-CSRF-Token']).toBeUndefined();
    expect(requests.some((r) => r.path === '/api/v1/session/csrf')).toBe(false);
    expect(ended).toBe(0);
  });

  it('sends a recovery code as recoveryCode and maps refusals', async () => {
    const { api, requests } = mock((r) => (r.path === '/api/v1/login/mfa' ? err(401, 'mfa_invalid') : undefined));
    const e = await api.loginMfa('ch-1', { recoveryCode: 'ABCD-EFGH-IJKL-MNOP' }).catch((x: unknown) => x);
    expect(requests[0]?.body).toEqual({ challengeId: 'ch-1', recoveryCode: 'ABCD-EFGH-IJKL-MNOP' });
    expect(loginMfaError(e)).toMatchObject({ restart: false });
    expect(loginMfaError(e).text).toMatch(/tidak valid/);
    expect(loginMfaError(new ApiError(410, 'mfa_challenge_expired', 'x'))).toMatchObject({ restart: true });
  });
});

describe('CP4c boot (TM-02 b)', () => {
  it('stops at enrolment for a role that needs MFA without an authenticator, before must_change', async () => {
    const { api, paths } = mock((r) => {
      if (r.path === '/api/v1/meta') return res(200, meta);
      if (r.path === '/api/v1/session') return res(200, session({ mfaEnrolled: false, mustChangePassword: true }));
      return undefined;
    });
    const boot = await loadBoot(api, signal());
    expect(boot.status).toBe('mfa_setup');
    expect(paths()).not.toContain('GET /api/v1/workspaces');
  });

  it('opens the workspace once enrolled; a role without the requirement is never stopped', async () => {
    for (const s of [session(), session({ mfaEnrolled: false, mfaRequired: false })]) {
      const { api } = mock((r) => {
        if (r.path === '/api/v1/meta') return res(200, meta);
        if (r.path === '/api/v1/session') return res(200, s);
        if (r.path === '/api/v1/workspaces') return res(200, { items: [] });
        return undefined;
      });
      expect((await loadBoot(api, signal())).status).toBe('ready');
    }
  });

  it('never stops at enrolment when the server has MFA off (ITSM_MFA default)', async () => {
    const { api } = mock((r) => {
      if (r.path === '/api/v1/meta') return res(200, meta);
      if (r.path === '/api/v1/session') return res(200, session({ mfaEnabled: false, mfaEnrolled: false }));
      if (r.path === '/api/v1/workspaces') return res(200, { items: [] });
      return undefined;
    });
    expect((await loadBoot(api, signal())).status).toBe('ready');
  });
});

describe('CP4c step-up retry (A-9)', () => {
  const resetRoute = (outcome: { stepUp: number }): Route => (r, n) => {
    if (r.path === '/api/v1/users/u-2/password-reset') return n === 1 ? err(401, 'step_up_required') : res(200, { temporaryPassword: 'synthetic-temp-1' });
    if (r.path === '/api/v1/session/step-up') { outcome.stepUp += 1; return res(204); }
    return csrfOk(r);
  };

  it('asks once, steps up, re-sends the original request exactly once and keeps the session', async () => {
    const outcome = { stepUp: 0 };
    let ended = 0;
    let prompts = 0;
    const { api, paths } = mock(resetRoute(outcome));
    api.onSessionEnded(() => { ended += 1; });
    api.onStepUpRequired(async () => { prompts += 1; await api.stepUp('123456'); return true; });
    const r = await api.resetUserPassword('u-2');
    expect(r.temporaryPassword).toBe('synthetic-temp-1');
    expect(prompts).toBe(1);
    expect(outcome.stepUp).toBe(1);
    expect(paths().filter((p) => p === 'POST /api/v1/users/u-2/password-reset')).toHaveLength(2);
    expect(ended).toBe(0);
  });

  it('cancel (or no prompt) returns step_up_required to the caller with nothing re-sent', async () => {
    for (const register of [true, false]) {
      const { api, paths } = mock(resetRoute({ stepUp: 0 }));
      if (register) api.onStepUpRequired(async () => false);
      const e = await api.resetUserPassword('u-2').catch((x: unknown) => x);
      expect((e as ApiError).code).toBe('step_up_required');
      expect(paths().filter((p) => p.endsWith('/password-reset'))).toHaveLength(1);
      expect(userWriteError(e, 'reset').text).toMatch(/Tidak ada yang berubah/);
    }
  });

  it('a second refusal after step-up is returned, never looped', async () => {
    let prompts = 0;
    const { api, paths } = mock((r) => (r.path === '/api/v1/users/u-2/unlock' ? err(401, 'step_up_required') : csrfOk(r)));
    api.onStepUpRequired(async () => { prompts += 1; return true; });
    const e = await api.unlockUser('u-2').catch((x: unknown) => x);
    expect((e as ApiError).code).toBe('step_up_required');
    expect(prompts).toBe(1);
    expect(paths().filter((p) => p.endsWith('/unlock'))).toHaveLength(2);
  });

  it('concurrent refusals share one prompt', async () => {
    let prompts = 0;
    let release: (ok: boolean) => void = () => undefined;
    const { api } = mock((r, n) => {
      if (r.path === '/api/v1/users/u-2/unlock' || r.path === '/api/v1/users/u-3/unlock') return n === 1 ? err(401, 'step_up_required') : res(204);
      return csrfOk(r);
    });
    api.onStepUpRequired(() => { prompts += 1; return new Promise<boolean>((resolve) => { release = resolve; }); });
    const both = Promise.all([api.unlockUser('u-2'), api.unlockUser('u-3')]);
    await new Promise((r) => setTimeout(r, 0));
    release(true);
    await both;
    expect(prompts).toBe(1);
  });

  it('a wrong step-up code is mfa_invalid, keeps the session and counts towards the lock text', async () => {
    let ended = 0;
    const { api } = mock((r) => (r.path === '/api/v1/session/step-up' ? err(401, 'mfa_invalid') : csrfOk(r)));
    api.onSessionEnded(() => { ended += 1; });
    const e = await api.stepUp('000000').catch((x: unknown) => x);
    expect((e as ApiError).code).toBe('mfa_invalid');
    expect(ended).toBe(0);
    expect(stepUpErrorText(e, 1)).toMatch(/tidak valid/);
    expect(stepUpErrorText(e, 5)).toMatch(/terkunci 15 menit/);
    expect(stepUpErrorText(new ApiError(409, 'mfa_not_enrolled', 'x'), 0)).toMatch(/Keamanan akun/);
  });
});

describe('CP4c own MFA and admin actions', () => {
  it('enrols with the CSRF token and validates the one-time recovery codes', async () => {
    const codes = Array.from({ length: 10 }, (_, i) => `AAAA-BBBB-CCCC-DDD${String.fromCharCode(65 + i)}`);
    const { api, requests } = mock((r) => {
      if (r.path === '/api/v1/me/mfa/enrollment') return res(201, { enrollmentId: 'en-1', otpauthUri: SYNTHETIC_URI, expiresAt: '2026-11-12T10:10:00Z' });
      if (r.path === '/api/v1/me/mfa/enrollment/en-1/confirm') return res(200, { recoveryCodes: codes });
      return csrfOk(r);
    });
    const en = await api.startMfaEnrollment();
    expect((await api.confirmMfaEnrollment(en.enrollmentId, '123456')).recoveryCodes).toHaveLength(10);
    expect(requests.find((r) => r.path.endsWith('/confirm'))?.headers['X-CSRF-Token']).toBe('csrf-1');

    const short = mock((r) => (r.path.endsWith('/recovery-codes') ? res(201, { recoveryCodes: codes.slice(1) }) : csrfOk(r)));
    expect(((await short.api.regenerateRecoveryCodes().catch((x: unknown) => x)) as ApiError).code).toBe('bad_response');
  });

  it('reads the otpauth URI without a QR library', () => {
    const p = parseOtpauth(SYNTHETIC_URI);
    expect(p).toEqual({ secret: 'JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP', issuer: 'DECA ITSM', account: 'sari' });
    expect(groupSecret(p!.secret)).toBe('JBSW Y3DP EHPK 3PXP JBSW Y3DP EHPK 3PXP');
    expect(parseOtpauth('https://example.invalid/?secret=AAAA')).toBeNull();
    expect(parseOtpauth('otpauth://totp/x?secret=not-base32!')).toBeNull();
    expect(isTotp('123 456')).toBe(true);
    expect(isTotp('12345')).toBe(false);
    expect(isRecoveryCode('abcd-efgh-ijkl-mnop')).toBe(true);
  });

  it('maps enrolment refusals', () => {
    expect(enrolError(new ApiError(422, 'validation_failed', 'x', null, 'code'))).toMatchObject({ restart: false, enrolled: false });
    expect(enrolError(new ApiError(410, 'mfa_enrollment_expired', 'x'))).toMatchObject({ restart: true });
    expect(enrolError(new ApiError(409, 'mfa_already_enrolled', 'x'))).toMatchObject({ enrolled: true });
  });

  it('resets another user\'s MFA with a reason and maps the refusals', async () => {
    const { api, requests } = mock((r) => (r.path === '/api/v1/users/u-2/mfa-reset' ? res(204) : csrfOk(r)));
    await api.resetUserMfa('u-2', 'Ponsel hilang');
    expect(requests.find((r) => r.path.endsWith('/mfa-reset'))?.body).toEqual({ reason: 'Ponsel hilang' });
    expect(validateMfaResetReason('  ')).toMatch(/wajib/);
    expect(validateMfaResetReason('x'.repeat(501))).toMatch(/500/);
    expect(validateMfaResetReason('Ponsel hilang')).toBeNull();
    expect(userWriteError(new ApiError(409, 'self_mfa_reset', 'x'), 'mfa_reset').text).toMatch(/Keamanan akun/);
    expect(userWriteError(new ApiError(422, 'validation_failed', 'x', null, 'reason'), 'mfa_reset').text).toMatch(/Alasan/);
  });

  it('turning own MFA off reports mfa_required_for_role as a conflict', async () => {
    const { api } = mock((r) => (r.path === '/api/v1/me/mfa' && r.method === 'DELETE' ? err(409, 'mfa_required_for_role') : csrfOk(r)));
    const e = await api.deleteMyMfa().catch((x: unknown) => x);
    expect((e as ApiError).code).toBe('mfa_required_for_role');
  });
});
