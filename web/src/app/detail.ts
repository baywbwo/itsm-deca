import type { TicketProperties, TicketPropertiesRequest } from '../api/types';

/** Editable D08a properties as form strings ('' = empty / no choice). */
export interface PropertiesForm {
  subject: string;
  chronology: string;
  priorityId: string;
  categoryId: string;
  productId: string;
  receivingUnitId: string;
  regionId: string;
  transactionActivity: string;
  cause: string;
  foreignBranchReference: string;
}

export const LIMITS = {
  subject: 300,
  chronology: 20000,
  transactionActivity: 20000,
  cause: 20000,
  foreignBranchReference: 100,
  relatedTitle: 300,
  relatedText: 5000,
  note: 5000,
} as const;

const ID_FIELDS = ['priorityId', 'categoryId', 'productId', 'receivingUnitId', 'regionId'] as const;
const TEXT_FIELDS = ['transactionActivity', 'cause', 'foreignBranchReference'] as const;
const ID_SOURCE: Record<(typeof ID_FIELDS)[number], keyof TicketProperties> = {
  priorityId: 'priority',
  categoryId: 'category',
  productId: 'product',
  receivingUnitId: 'receivingUnit',
  regionId: 'region',
};

export function formFromProps(p: TicketProperties): PropertiesForm {
  const id = (k: keyof TicketProperties) => {
    const v = p[k];
    return v && typeof v === 'object' ? v.id : '';
  };
  return {
    subject: p.subject,
    chronology: p.chronology,
    priorityId: id('priority'),
    categoryId: id('category'),
    productId: id('product'),
    receivingUnitId: id('receivingUnit'),
    regionId: id('region'),
    transactionActivity: p.transactionActivity ?? '',
    cause: p.cause ?? '',
    foreignBranchReference: p.foreignBranchReference ?? '',
  };
}

/**
 * Only the fields that differ from what the server last sent (trimmed text; '' clears an
 * optional field with null). `null` when nothing changed. `version` is the version the edit
 * started from, so a concurrent change answers 409 instead of being overwritten.
 */
export function diffProps(f: PropertiesForm, p: TicketProperties, version: number): TicketPropertiesRequest | null {
  const base = formFromProps(p);
  const body: TicketPropertiesRequest = { version };
  let changed = false;
  for (const k of ['subject', 'chronology'] as const) {
    if (f[k].trim() !== base[k].trim()) { body[k] = f[k].trim(); changed = true; }
  }
  for (const k of ID_FIELDS) {
    if (f[k] !== base[k]) { body[k] = f[k] || null; changed = true; }
  }
  for (const k of TEXT_FIELDS) {
    if (f[k].trim() !== base[k].trim()) { body[k] = f[k].trim() || null; changed = true; }
  }
  return changed ? body : null;
}

export type PropertiesError = { field: keyof PropertiesForm; message: string };

export function validateProps(f: PropertiesForm): PropertiesError[] {
  const out: PropertiesError[] = [];
  if (!f.subject.trim()) out.push({ field: 'subject', message: 'Subjek wajib diisi.' });
  if (!f.chronology.trim()) out.push({ field: 'chronology', message: 'Kronologi wajib diisi.' });
  for (const k of ['subject', 'chronology', ...TEXT_FIELDS] as const) {
    if ([...f[k].trim()].length > LIMITS[k]) out.push({ field: k, message: `Maksimal ${LIMITS[k]} karakter.` });
  }
  return out;
}

/** Server field name → form field (both use the API names). */
export function propsField(apiField: string | null | undefined): keyof PropertiesForm | null {
  const all: readonly string[] = ['subject', 'chronology', ...ID_FIELDS, ...TEXT_FIELDS];
  return apiField && all.includes(apiField) ? (apiField as keyof PropertiesForm) : null;
}

export const idSource = (k: (typeof ID_FIELDS)[number]) => ID_SOURCE[k];

/**
 * Unsaved property edits, in memory only (never storage: chronology can carry complaint text).
 * Keyed by workspace + ticket so closing the peek or going Back does not drop them silently, and
 * a draft never follows the user into another workspace.
 */
export interface PropertiesDraft { form: PropertiesForm; version: number }
const store = new Map<string, PropertiesDraft>();
const key = (ws: string, ticket: string) => `${ws}|${ticket}`;
export const propertyDrafts = {
  get: (ws: string, ticket: string) => store.get(key(ws, ticket)),
  set: (ws: string, ticket: string, d: PropertiesDraft) => { store.set(key(ws, ticket), d); },
  clear: (ws: string, ticket: string) => { store.delete(key(ws, ticket)); },
};
