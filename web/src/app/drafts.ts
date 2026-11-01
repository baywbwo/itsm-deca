import type { TicketCreateRequest } from '../api/types';

/**
 * D07 intake drafts, kept in memory only (never localStorage: the chronology may hold personal
 * data). One draft per user + workspace; a reload or sign-out drops it. Each draft owns its
 * idempotency key so a resend after a timeout replays instead of creating a second ticket.
 */
export interface IntakeForm {
  subject: string;
  chronology: string;
  /** `YYYY-MM-DDTHH:mm`, wall time in the workspace zone. */
  receivedLocal: string;
  intakeChannelId: string;
  priorityId: string;
  categoryId: string;
  productId: string;
  receivingUnitId: string;
  regionId: string;
  merchantId: string;
  ownerUserId: string;
  jiraMode: 'none' | 'create' | 'link';
  jiraTitle: string;
  jiraIssueKey: string;
  jiraAssigneeId: string;
  larkDestinationId: string;
}

export interface IntakeDraft {
  form: IntakeForm;
  /** Snapshot when the draft was started; `dirty` compares against it. */
  initial: IntakeForm;
  key: string;
  /**
   * Set while the outcome of a send is unknown (timeout/network/5xx): the exact payload that
   * went out. A resend must use the same key and this payload; the form is locked meanwhile.
   */
  inFlight: TicketCreateRequest | null;
}

export const emptyForm = (receivedLocal: string): IntakeForm => ({
  subject: '', chronology: '', receivedLocal, intakeChannelId: '', priorityId: '', categoryId: '',
  productId: '', receivingUnitId: '', regionId: '', merchantId: '', ownerUserId: '',
  jiraMode: 'none', jiraTitle: '', jiraIssueKey: '', jiraAssigneeId: '', larkDestinationId: '',
});

export function newKey(): string {
  const c = globalThis.crypto;
  if (c?.randomUUID) return `intake-${c.randomUUID()}`;
  const b = new Uint8Array(16);
  c.getRandomValues(b);
  return `intake-${Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')}`;
}

export function isDirty(d: IntakeDraft | undefined): boolean {
  if (!d) return false;
  if (d.inFlight) return true;
  return (Object.keys(d.form) as (keyof IntakeForm)[]).some((k) => d.form[k] !== d.initial[k]);
}

const store = new Map<string, IntakeDraft>();
const scope = (userId: string, workspaceId: string) => `${userId}:${workspaceId}`;

export const drafts = {
  get: (userId: string, workspaceId: string) => store.get(scope(userId, workspaceId)),
  put: (userId: string, workspaceId: string, d: IntakeDraft) => { store.set(scope(userId, workspaceId), d); },
  discard: (userId: string, workspaceId: string) => { store.delete(scope(userId, workspaceId)); },
  dirty: (userId: string, workspaceId: string) => isDirty(store.get(scope(userId, workspaceId))),
  anyDirty: () => [...store.values()].some(isDirty),
  clear: () => store.clear(),
};

/** Wall time `YYYY-MM-DDTHH:mm` of instant `t` in `timeZone`. */
export function wallTime(t: number, timeZone: string): string {
  const p = new Intl.DateTimeFormat('en-CA', {
    timeZone, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  }).formatToParts(t);
  const v = (type: string) => p.find((x) => x.type === type)?.value ?? '';
  return `${v('year')}-${v('month')}-${v('day')}T${v('hour')}:${v('minute')}`;
}

/**
 * Wall time in `timeZone` → RFC 3339 with that zone's offset at that moment (Jakarta: +07:00,
 * no DST). Returns null for malformed input.
 */
export function wallToRfc3339(local: string, timeZone: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(local);
  if (!m) return null;
  const asUtc = Date.UTC(+m[1]!, +m[2]! - 1, +m[3]!, +m[4]!, +m[5]!);
  // Offset at the guessed instant; one correction step covers zones with DST changes.
  let off = offsetMinutes(asUtc, timeZone);
  off = offsetMinutes(asUtc - off * 60000, timeZone);
  const sign = off >= 0 ? '+' : '-';
  const a = Math.abs(off);
  const hh = String(Math.floor(a / 60)).padStart(2, '0');
  const mm = String(a % 60).padStart(2, '0');
  return `${m[1]}-${m[2]}-${m[3]}T${m[4]}:${m[5]}:00${sign}${hh}:${mm}`;
}

function offsetMinutes(t: number, timeZone: string): number {
  const p = new Intl.DateTimeFormat('en-US', {
    timeZone, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit',
  }).formatToParts(t);
  const n = (type: string) => Number(p.find((x) => x.type === type)?.value);
  const asUtc = Date.UTC(n('year'), n('month') - 1, n('day'), n('hour'), n('minute'), n('second'));
  return Math.round((asUtc - Math.floor(t / 1000) * 1000) / 60000);
}

export type IntakeFieldError = { field: keyof IntakeForm; message: string };

/** Client checks mirroring the server (the server stays the authority). */
export function validateIntake(f: IntakeForm): IntakeFieldError[] {
  const errs: IntakeFieldError[] = [];
  if (!f.subject.trim()) errs.push({ field: 'subject', message: 'Judul pengaduan wajib diisi.' });
  else if ([...f.subject.trim()].length > 300) errs.push({ field: 'subject', message: 'Judul maksimal 300 karakter.' });
  if (!f.receivedLocal) errs.push({ field: 'receivedLocal', message: 'Waktu diterima wajib diisi.' });
  if (!f.intakeChannelId) errs.push({ field: 'intakeChannelId', message: 'Kanal penerimaan wajib dipilih.' });
  if (!f.chronology.trim()) errs.push({ field: 'chronology', message: 'Kronologi wajib diisi.' });
  else if ([...f.chronology.trim()].length > 20000) errs.push({ field: 'chronology', message: 'Kronologi maksimal 20.000 karakter.' });
  if (f.jiraMode === 'create' && !f.jiraTitle.trim()) errs.push({ field: 'jiraTitle', message: 'Judul issue Jira wajib untuk membuat issue.' });
  if (f.jiraMode === 'link' && !f.jiraIssueKey.trim()) errs.push({ field: 'jiraIssueKey', message: 'Kunci issue wajib untuk menautkan.' });
  return errs;
}

/** Form → request body. Empty optional selects are omitted; company is the workspace path. */
export function toRequest(f: IntakeForm, timeZone: string): TicketCreateRequest {
  const body: TicketCreateRequest = {
    subject: f.subject.trim(),
    chronology: f.chronology.trim(),
    receivedAt: wallToRfc3339(f.receivedLocal, timeZone) ?? f.receivedLocal,
    intakeChannelId: f.intakeChannelId,
  };
  const optional = ['priorityId', 'categoryId', 'productId', 'receivingUnitId', 'regionId', 'merchantId',
    'ownerUserId', 'larkDestinationId'] as const;
  for (const k of optional) if (f[k]) body[k] = f[k];
  if (f.jiraMode === 'create') {
    body.jira = { mode: 'create', title: f.jiraTitle.trim(), ...(f.jiraAssigneeId ? { assigneeId: f.jiraAssigneeId } : {}) };
  } else if (f.jiraMode === 'link') {
    body.jira = {
      mode: 'link', issueKey: f.jiraIssueKey.trim(),
      ...(f.jiraTitle.trim() ? { title: f.jiraTitle.trim() } : {}),
      ...(f.jiraAssigneeId ? { assigneeId: f.jiraAssigneeId } : {}),
    };
  }
  return body;
}

/** Server error field (`jira.title`, `ownerUserId`, …) → form field. */
export function formField(apiField: string | undefined): keyof IntakeForm | null {
  switch (apiField) {
    case 'subject': case 'chronology': case 'intakeChannelId': case 'priorityId': case 'categoryId':
    case 'productId': case 'receivingUnitId': case 'regionId': case 'merchantId': case 'ownerUserId':
    case 'larkDestinationId':
      return apiField;
    case 'receivedAt': return 'receivedLocal';
    case 'jira.title': return 'jiraTitle';
    case 'jira.issueKey': return 'jiraIssueKey';
    case 'jira.assigneeId': return 'jiraAssigneeId';
    case 'jira': case 'jira.mode': return 'jiraMode';
    default: return null;
  }
}
