import { describe, expect, it } from 'vitest';
import type { SsoProvider } from '../api/types';
import {
  SSO_COMPANY_KEY, awaitingAccess, emptySsoDraft, rememberSsoCompany, rememberedSsoCompany, splitList, ssoBody,
  ssoCompanyCode, ssoDraftOf, ssoEnableBlocker, ssoLoginNotice, ssoMoves, ssoStartPath, ssoState, ssoTestText, takeSsoResult,
} from './sso';

const provider = (over: Partial<SsoProvider> = {}): SsoProvider => ({
  id: 'p-1', displayName: 'Entra', issuer: 'https://login.example.com/t/v2.0', clientId: 'client-1', secretSet: true,
  secretUpdatedAt: null, emailDomains: ['dob.co.id'], extraHosts: [], groupClaim: 'groups',
  roleMapping: [{ group: 'cs-team', role: 'cs' }], syncRoles: false, enabled: false, lastTestOk: true, lastTestedAt: null, version: 3,
  ...over,
});

describe('CP5c SSO login entry', () => {
  it('company code: 2–10 letters/digits, upper-cased, same rule as the server', () => {
    expect(ssoCompanyCode(' dob ')).toBe('DOB');
    expect(ssoCompanyCode('AJR1')).toBe('AJR1');
    for (const bad of ['', 'D', 'DOB-1', 'ABCDEFGHIJK', 'DO B', '../x']) expect(ssoCompanyCode(bad)).toBeNull();
    expect(ssoStartPath('DOB')).toBe('/api/v1/auth/oidc/DOB/start');
  });

  it('callback codes read as words; unknown codes stay generic; no code = no notice', () => {
    expect(ssoLoginNotice(null)).toBeNull();
    expect(ssoLoginNotice('not_linked')?.text).toContain('belum terhubung');
    expect(ssoLoginNotice('expired')?.text).toContain('kedaluwarsa');
    expect(ssoLoginNotice('<script>')?.text).toBe('Proses masuk tidak selesai. Coba lagi atau hubungi admin platform.');
    for (const c of ['not_configured', 'unavailable', 'idp_unavailable', 'idp_error', 'denied', 'inactive', 'email_in_use']) {
      expect(ssoLoginNotice(c)?.text).not.toContain('Proses masuk tidak selesai');
    }
  });

  it('?sso= is read once and removed from the address, other params and the hash stay', () => {
    const urls: string[] = [];
    expect(takeSsoResult({ search: '?sso=expired&x=1', pathname: '/', hash: '#/w/1/dashboard' }, (u) => urls.push(u))).toBe('expired');
    expect(urls).toEqual(['/?x=1#/w/1/dashboard']);
    expect(takeSsoResult({ search: '?sso=denied', pathname: '/', hash: '' }, (u) => urls.push(u))).toBe('denied');
    expect(urls[1]).toBe('/');
    expect(takeSsoResult({ search: '', pathname: '/', hash: '' }, (u) => urls.push(u))).toBeNull();
    expect(urls).toHaveLength(2);
  });

  it('remembers only a valid company code; broken storage is ignored', () => {
    const data = new Map<string, string>();
    const store = { getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => { data.set(k, v); } };
    rememberSsoCompany('DOB', store);
    expect(data.get(SSO_COMPANY_KEY)).toBe('DOB');
    expect(rememberedSsoCompany(store)).toBe('DOB');
    data.set(SSO_COMPANY_KEY, 'not a code!');
    expect(rememberedSsoCompany(store)).toBe('');
    const broken = { getItem: () => { throw new Error('denied'); }, setItem: () => { throw new Error('denied'); } };
    expect(rememberedSsoCompany(broken)).toBe('');
    expect(() => rememberSsoCompany('DOB', broken)).not.toThrow();
  });

  it('an account without a company waits; platform admins and UAT keep the shell', () => {
    expect(awaitingAccess({ platformAdmin: false, uatSuperadmin: false }, [])).toBe(true);
    expect(awaitingAccess({ platformAdmin: true, uatSuperadmin: false }, [])).toBe(false);
    expect(awaitingAccess({ platformAdmin: false, uatSuperadmin: true }, [])).toBe(false);
    expect(awaitingAccess({ platformAdmin: false, uatSuperadmin: false }, [{}])).toBe(false);
  });
});

describe('CP5c SSO administration', () => {
  it('lists split on lines and commas, trimmed, lower-cased, de-duplicated', () => {
    expect(splitList(' DOB.co.id\nbmt.co.id, dob.co.id ,,\n')).toEqual(['dob.co.id', 'bmt.co.id']);
    expect(splitList('')).toEqual([]);
  });

  it('body: trimmed fields, empty mapping rows dropped, version passed through (null = create)', () => {
    const d = { ...emptySsoDraft(), displayName: ' Entra ', issuer: ' https://i.example ', clientId: ' c ', emailDomains: 'A.com',
      roleMapping: [{ group: ' leads ', role: 'lead_cs' as const }, { group: '  ', role: 'cs' as const }] };
    expect(ssoBody(d, null)).toEqual({
      displayName: 'Entra', issuer: 'https://i.example', clientId: 'c', emailDomains: ['a.com'], extraHosts: [],
      groupClaim: 'groups', roleMapping: [{ group: 'leads', role: 'lead_cs' }], syncRoles: false, version: null,
    });
    expect(ssoBody(ssoDraftOf(provider()), 3).version).toBe(3);
  });

  it('only issuer, client id or hosts count as a move (needs a new test); names and mapping do not', () => {
    const p = provider({ extraHosts: ['oauth2.googleapis.com'] });
    const d = ssoDraftOf(p);
    expect(ssoMoves(p, d)).toBe(false);
    expect(ssoMoves(p, { ...d, displayName: 'Lain', roleMapping: [], emailDomains: 'x.com' })).toBe(false);
    expect(ssoMoves(p, { ...d, extraHosts: 'OAUTH2.googleapis.com\n' })).toBe(false);
    expect(ssoMoves(p, { ...d, issuer: 'https://other.example' })).toBe(true);
    expect(ssoMoves(p, { ...d, clientId: 'client-2' })).toBe(true);
    expect(ssoMoves(p, { ...d, extraHosts: '' })).toBe(true);
  });

  it('state and the first thing blocking activation', () => {
    expect(ssoState(null, true)).toBe('none');
    expect(ssoState(provider({ enabled: true }), true)).toBe('on');
    expect(ssoState(provider(), true)).toBe('ready');
    expect(ssoState(provider(), false)).toBe('draft');
    expect(ssoEnableBlocker(provider(), false)).toContain('ITSM_KEYRING');
    expect(ssoEnableBlocker(provider({ secretSet: false, lastTestOk: null }), true)).toContain('client secret');
    expect(ssoEnableBlocker(provider({ lastTestOk: false }), true)).toContain('Uji koneksi');
    expect(ssoEnableBlocker(provider(), true)).toBeNull();
  });

  it('test result in words, with the server code but never IdP text', () => {
    expect(ssoTestText({ ok: true, stage: null, code: null })).toContain('berhasil');
    expect(ssoTestText({ ok: false, stage: 'policy', code: 'host_not_allowed' })).toContain('kebijakan host (host_not_allowed)');
    expect(ssoTestText({ ok: false, stage: 'jwks', code: null })).toContain('JWKS');
    expect(ssoTestText({ ok: false, stage: 'discovery', code: 'http_503' })).toContain('discovery gagal dibaca (http_503)');
  });
});
