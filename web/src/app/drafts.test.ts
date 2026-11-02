import { describe, expect, it } from 'vitest';
import { drafts, emptyForm, formField, isDirty, newKey, toRequest, validateIntake, wallTime, wallToRfc3339, type IntakeForm } from './drafts';

const filled = (over: Partial<IntakeForm> = {}): IntakeForm => ({
  ...emptyForm('2026-11-01T09:30'),
  subject: '  Saldo terpotong  ', chronology: ' Kronologi dummy ', intakeChannelId: 'ch-1', ...over,
});

describe('D07 intake drafts', () => {
  it('persists legacy contact fields in chronology without adding unsupported request fields', () => {
    const body = toRequest(filled({ merchantName: ' Toko Uji ', reporterName: 'PIC Uji', reporterEmail: 'test@example.com', reporterPhone: '08123456789' }), 'UTC');
    expect(body.chronology).toBe('Merchant: Toko Uji\nPelapor: PIC Uji\nEmail terdaftar: test@example.com\nWhatsApp: 08123456789\n\nKronologi dummy');
    expect(body).not.toHaveProperty('reporterEmail');
    expect(body).not.toHaveProperty('jira');
    expect(validateIntake(filled({ reporterEmail: 'invalid' })).some(e => e.field === 'reporterEmail')).toBe(true);
    expect(validateIntake(filled({ chronology: 'x'.repeat(20000), merchantName: 'Toko Uji' })).some(e => e.field === 'chronology')).toBe(true);
  });

  it('received time is Jakarta wall time with +07:00', () => {
    expect(wallToRfc3339('2026-11-01T09:30', 'Asia/Jakarta')).toBe('2026-11-01T09:30:00+07:00');
    expect(wallToRfc3339('2026-11-01T00:05', 'UTC')).toBe('2026-11-01T00:05:00+00:00');
    expect(wallToRfc3339('01/11/2026 09:30', 'Asia/Jakarta')).toBeNull();
    expect(wallTime(Date.UTC(2026, 10, 1, 17, 30), 'Asia/Jakarta')).toBe('2026-11-02T00:30');
  });

  it('title, chronology, channel and received time are required; jira mode needs its field', () => {
    const fields = validateIntake(emptyForm('')).map((e) => e.field);
    expect(fields).toEqual(['subject', 'receivedLocal', 'intakeChannelId', 'chronology']);
    expect(validateIntake(filled())).toEqual([]);
    expect(validateIntake(filled({ subject: 'x'.repeat(301) }))[0]?.field).toBe('subject');
    expect(validateIntake(filled({ jiraMode: 'create' }))[0]?.field).toBe('jiraTitle');
    expect(validateIntake(filled({ jiraMode: 'link' }))[0]?.field).toBe('jiraIssueKey');
    // Optional catalog fields are not made mandatory by the client.
    expect(validateIntake(filled({ priorityId: '', categoryId: '', regionId: '' }))).toEqual([]);
  });

  it('request omits empty optionals, trims text, carries jira only when chosen', () => {
    const body = toRequest(filled({ priorityId: 'p1', ownerUserId: '' }), 'Asia/Jakarta');
    expect(body).toEqual({
      subject: 'Saldo terpotong', chronology: 'Kronologi dummy', receivedAt: '2026-11-01T09:30:00+07:00',
      intakeChannelId: 'ch-1', priorityId: 'p1',
    });
    const jc = toRequest(filled({ jiraMode: 'create', jiraTitle: ' Issue ', jiraAssigneeId: 'a1', larkDestinationId: 'l1' }), 'Asia/Jakarta');
    expect(jc.jira).toEqual({ mode: 'create', title: 'Issue', assigneeId: 'a1' });
    expect(jc.larkDestinationId).toBe('l1');
    const jl = toRequest(filled({ jiraMode: 'link', jiraIssueKey: ' SIM-1 ' }), 'Asia/Jakarta');
    expect(jl.jira).toEqual({ mode: 'link', issueKey: 'SIM-1' });
  });

  it('server field names map back to form fields', () => {
    expect(formField('receivedAt')).toBe('receivedLocal');
    expect(formField('jira.issueKey')).toBe('jiraIssueKey');
    expect(formField('ownerUserId')).toBe('ownerUserId');
    expect(formField('unknown')).toBeNull();
  });

  it('dirty compares to the initial snapshot; an unknown send stays dirty', () => {
    const initial = emptyForm('2026-11-01T09:30');
    expect(isDirty({ form: initial, initial, key: 'k', inFlight: null })).toBe(false);
    expect(isDirty({ form: { ...initial, subject: 'a' }, initial, key: 'k', inFlight: null })).toBe(true);
    expect(isDirty({ form: initial, initial, key: 'k', inFlight: toRequest(filled(), 'UTC') })).toBe(true);
  });

  it('store is scoped per user + workspace and memory only', () => {
    drafts.clear();
    const initial = emptyForm('');
    drafts.put('u1', 'w1', { form: { ...initial, subject: 'x' }, initial, key: 'k1', inFlight: null });
    expect(drafts.dirty('u1', 'w1')).toBe(true);
    expect(drafts.dirty('u1', 'w2')).toBe(false);
    expect(drafts.dirty('u2', 'w1')).toBe(false);
    expect(drafts.anyDirty()).toBe(true);
    drafts.discard('u1', 'w1');
    expect(drafts.anyDirty()).toBe(false);
    expect(globalThis.localStorage?.length ?? 0).toBe(0);
  });

  it('idempotency keys are random and prefixed', () => {
    const a = newKey();
    expect(a).toMatch(/^intake-[0-9a-f-]{32,36}$/);
    expect(newKey()).not.toBe(a);
  });
});
