import { describe, expect, it } from 'vitest';
import type { Workspace } from '../api/types';
import { freshest } from './workspaces';

const ws: Workspace = {
  id: 'a1e8dd4a-a301-42f1-ac2a-58db5c57811b', code: 'DUMA', name: 'Workspace A (dummy)',
  timezone: 'Asia/Jakarta', companyId: 'c0000000-0000-4000-8000-000000000001', version: 3, myRole: 'cs',
};

describe('freshest', () => {
  it('takes a newer name and timezone from the list but keeps the scoped role', () => {
    const listed: Workspace = { ...ws, name: 'Workspace A baru (dummy)', timezone: 'Asia/Makassar', version: 4, myRole: 'lead_cs' };
    expect(freshest(ws, listed)).toEqual({ ...ws, name: 'Workspace A baru (dummy)', timezone: 'Asia/Makassar', version: 4 });
  });
  it('ignores an older or equal list entry, another workspace and a missing one', () => {
    expect(freshest(ws, { ...ws, name: 'lama', version: 2 })).toBe(ws);
    expect(freshest(ws, { ...ws, name: 'sama', version: 3 })).toBe(ws);
    expect(freshest(ws, { ...ws, id: 'ecc3519f-a894-4269-adbc-64ee540a0e28', version: 9 })).toBe(ws);
    expect(freshest(ws, undefined)).toBe(ws);
    expect(freshest(ws, null)).toBe(ws);
  });
});
