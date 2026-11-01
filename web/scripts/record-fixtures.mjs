#!/usr/bin/env node
// Records real responses from a running UAT backend (dummy data) for the static review
// package. The CSRF token and cookies are never written; the token is replaced by a marker.
import { writeFileSync } from 'node:fs';

const BASE = process.env.ITSM_WEB_ORIGIN ?? 'http://127.0.0.1:4183';
const OUT = new URL('../src/review/fixtures.json', import.meta.url);
let cookie = '';

async function rec(method, path, headers = {}, body = undefined) {
  const res = await fetch(BASE + path, {
    method,
    headers: { Accept: 'application/json', ...(cookie && { Cookie: cookie }), ...(body && { 'Content-Type': 'application/json' }), ...headers },
    ...(body && { body: JSON.stringify(body) }),
  });
  const set = res.headers.getSetCookie();
  if (set.length) cookie = set.map((c) => c.split(';')[0]).join('; ');
  const text = await res.text();
  return { status: res.status, body: text ? JSON.parse(text) : null, requestId: res.headers.get('x-request-id') };
}

const responses = {};
responses['GET /api/v1/meta'] = await rec('GET', '/api/v1/meta');
responses['GET /api/v1/session#anonymous'] = await rec('GET', '/api/v1/session');
const entry = await rec('POST', '/api/v1/uat/session', { 'X-ITSM-UAT-Entry': '1' });
if (entry.status !== 201) throw new Error(`UAT entry failed: ${entry.status}`);
const csrf = entry.body.csrfToken;
entry.body.csrfToken = 'rekaman-token-disensor';
responses['POST /api/v1/uat/session'] = entry;
responses['GET /api/v1/session'] = await rec('GET', '/api/v1/session');
const list = await rec('GET', '/api/v1/workspaces');
responses['GET /api/v1/workspaces'] = list;
for (const w of list.body.items) {
  responses[`GET /api/v1/workspaces/${w.id}`] = await rec('GET', `/api/v1/workspaces/${w.id}`);
  responses[`GET /api/v1/workspaces/${w.id}/permissions`] = await rec('GET', `/api/v1/workspaces/${w.id}/permissions`);
  let options = null;
  for (const period of ['today', 'last_7_days', 'this_month']) {
    const path = `/api/v1/workspaces/${w.id}/dashboard?period=${period}`;
    responses[`GET ${path}`] = await rec('GET', path);
    options ??= responses[`GET ${path}`].body.filterOptions;
  }
  // Every single filter value (plus the unset word) for every period, in the canonical query
  // order the app sends. Combinations are not recorded; the replay says so instead of guessing.
  const singles = [
    ...options.owners.map((o) => ['owner', o.id]), ['owner', 'unassigned'],
    ...options.channels.map((o) => ['channel', o.id]), ['channel', 'none'],
    ...options.categories.map((o) => ['category', o.id]), ['category', 'none'],
    ...options.priorities.map((o) => ['priority', o.id]), ['priority', 'none'],
  ];
  for (const period of ['today', 'last_7_days', 'this_month']) {
    for (const [k, v] of singles) {
      const path = `/api/v1/workspaces/${w.id}/dashboard?${new URLSearchParams({ period, [k]: v })}`;
      responses[`GET ${path}`] = await rec('GET', path);
    }
  }
  // Combined filters for the visual review (today only): every PIC x priority and channel x category.
  const pairs = [
    ...options.owners.flatMap((o) => options.priorities.map((p) => ({ owner: o.id, priority: p.id }))),
    ...options.channels.flatMap((c) => options.categories.map((k) => ({ channel: c.id, category: k.id }))),
  ];
  for (const p of pairs) {
    const q = new URLSearchParams({ period: 'today' });
    for (const k of ['owner', 'channel', 'category', 'priority']) if (p[k]) q.set(k, p[k]);
    const path = `/api/v1/workspaces/${w.id}/dashboard?${q}`;
    responses[`GET ${path}`] = await rec('GET', path);
  }
  // D06 tickets: List + Board for the default scope, every single status/PIC/priority filter and
  // every dashboard drilldown (today), plus the next page of the List and of every Board column.
  const base = `/api/v1/workspaces/${w.id}`;
  responses[`GET ${base}/members`] = await rec('GET', `${base}/members`);
  responses[`GET ${base}/catalogs`] = await rec('GET', `${base}/catalogs`);
  responses[`GET ${base}/intake-directory`] = await rec('GET', `${base}/intake-directory`);
  const today = responses[`GET ${base}/dashboard?period=today`].body;
  const order = ['status', 'owner', 'channel', 'category', 'priority', 'receivedFrom', 'receivedBefore', 'resolvedFrom', 'resolvedBefore'];
  const toQuery = (f) => {
    const q = new URLSearchParams();
    for (const k of order) {
      const v = f[k];
      if (Array.isArray(v) ? v.length : v) q.set(k, Array.isArray(v) ? v.join(',') : v);
    }
    return q.toString();
  };
  const scopes = new Set(['', 'sort=number&order=asc']);
  for (const s of ['new', 'open', 'pending_customer', 'pending_engineering', 'resolved', 'closed']) scopes.add(`status=${s}`);
  for (const o of [...options.owners.map((o) => o.id), 'unassigned']) scopes.add(toQuery({ owner: o }));
  for (const o of [...options.priorities.map((o) => o.id), 'none']) scopes.add(toQuery({ priority: o }));
  for (const f of Object.values(today.drilldown)) if (f) scopes.add(toQuery(f));
  for (const r of today.statusDistribution) scopes.add(toQuery(r.filter));
  const qs = (q, extra = {}) => { const u = new URLSearchParams(q); for (const [k, v] of Object.entries(extra)) u.set(k, v); const t = u.toString(); return t ? `?${t}` : ''; };
  for (const q of scopes) {
    const l = `${base}/tickets${qs(q)}`;
    responses[`GET ${l}`] = await rec('GET', l);
    const next = responses[`GET ${l}`].body?.nextCursor;
    if (next) responses[`GET ${base}/tickets${qs(q, { cursor: next })}`] = await rec('GET', `${base}/tickets${qs(q, { cursor: next })}`);
    const b = `${base}/tickets/board${qs(q)}`;
    responses[`GET ${b}`] = await rec('GET', b);
    for (const c of responses[`GET ${b}`].body?.columns ?? []) {
      if (!c.nextCursor) continue;
      const more = `${base}/tickets${qs(q, { status: c.status.code, cursor: c.nextCursor })}`;
      responses[`GET ${more}`] = await rec('GET', more);
    }
  }
  // A filter value from outside the workspace: the server's real 400.
  const bad = `/api/v1/workspaces/${w.id}/dashboard?period=today&owner=00000000-0000-4000-8000-000000000000`;
  responses[`GET ${bad}`] = await rec('GET', bad);
}
// Mutations (recorded last so every GET above is the "before" state). One stale version gives the
// server's real 409; one real move new -> open (dummy DB) gives the 200, followed by the "after"
// List/Board so the replay can show the refreshed scope.
const first = list.body.items[0];
const fb = `/api/v1/workspaces/${first.id}`;
const candidate = responses[`GET ${fb}/tickets?status=new`].body.items
  .find((t) => t.allowedActions.some((a) => a.to.code === 'open' && a.requires === 'none' && !a.blockedBy));
if (candidate) {
  const path = `${fb}/tickets/${candidate.id}/transitions`;
  responses[`POST ${path}#stale`] = await rec('POST', path, { 'X-CSRF-Token': csrf }, { to: 'open', version: candidate.version - 1 });
  responses[`POST ${path}#open`] = await rec('POST', path, { 'X-CSRF-Token': csrf }, { to: 'open', version: candidate.version });
  for (const p of [`${fb}/tickets`, `${fb}/tickets/board`, `${fb}/tickets?status=new`, `${fb}/tickets/board?status=new`]) {
    responses[`GET ${p}#after`] = await rec('GET', p);
  }
}
// D07 intake (first workspace, dummy DB): a real create with simulated Jira create + Lark, its
// same-key replay, a same-key different-payload 409, then a create while the simulation is set
// to Jira unknown / Lark failed. Keys are fixed per recording; the replay picks by call order.
{
  const dir = responses[`GET ${fb}/intake-directory`].body;
  const cat = responses[`GET ${fb}/catalogs`].body;
  const members = responses[`GET ${fb}/members`].body.items.filter((m) => m.active);
  const stamp = Date.now();
  const at = new Date(stamp - 15 * 60000);
  const local = new Date(at.getTime() + 7 * 3600000).toISOString().slice(0, 16);
  const body = {
    subject: 'Saldo terpotong dua kali (contoh dummy)',
    chronology: 'Contoh kronologi dummy untuk paket review D07; bukan data nasabah.',
    receivedAt: `${local}:00+07:00`,
    intakeChannelId: cat.intakeChannels.find((c) => c.active).id,
    priorityId: cat.priorities.find((c) => c.active).id,
    ownerUserId: members[0].userId,
    merchantId: dir.merchants[0].id,
    jira: { mode: 'create', title: 'Investigasi potongan ganda (dummy)', assigneeId: dir.jira.options[0].id },
    larkDestinationId: dir.lark.options[0].id,
  };
  const h = (key) => ({ 'X-CSRF-Token': csrf, 'Idempotency-Key': key });
  const sim = async (conn, outcome) => {
    const now = (await rec('GET', `${fb}/intake-directory`)).body;
    const c = now[conn];
    await rec('PUT', `${fb}/integrations/${c.connectionId}/simulation`, { 'X-CSRF-Token': csrf }, { outcome, version: c.version });
  };
  await sim('jira', 'success');
  await sim('lark', 'success');
  const k1 = `review-${stamp}-1`;
  responses[`POST ${fb}/tickets#created`] = await rec('POST', `${fb}/tickets`, h(k1), body);
  responses[`POST ${fb}/tickets#replayed`] = await rec('POST', `${fb}/tickets`, h(k1), body);
  responses[`POST ${fb}/tickets#conflict`] = await rec('POST', `${fb}/tickets`, h(k1), { ...body, subject: `${body.subject} (diubah)` });
  const t1 = responses[`POST ${fb}/tickets#created`].body.ticket;
  responses[`GET ${fb}/tickets/${t1.id}/integrations`] = await rec('GET', `${fb}/tickets/${t1.id}/integrations`);
  responses[`GET ${fb}/tickets/${t1.id}/assignment-history`] = await rec('GET', `${fb}/tickets/${t1.id}/assignment-history`);
  const q1 = `${fb}/tickets?${new URLSearchParams({ q: t1.displayNumber })}`;
  responses[`GET ${q1}`] = await rec('GET', q1);
  await sim('jira', 'unknown');
  await sim('lark', 'failed');
  const k2 = `review-${stamp}-2`;
  responses[`POST ${fb}/tickets#degraded`] = await rec('POST', `${fb}/tickets`, h(k2), { ...body, subject: 'Notifikasi gagal terkirim (contoh dummy)' });
  const t2 = responses[`POST ${fb}/tickets#degraded`].body.ticket;
  responses[`GET ${fb}/tickets/${t2.id}/integrations`] = await rec('GET', `${fb}/tickets/${t2.id}/integrations`);
  const q2 = `${fb}/tickets?${new URLSearchParams({ q: t2.displayNumber })}`;
  responses[`GET ${q2}`] = await rec('GET', q2);
  await sim('jira', 'success');
  await sim('lark', 'success');
  // Assignment on an existing ticket: history, a stale version (real 409), then a real reassign.
  const owned = (responses[`GET ${fb}/tickets#after`] ?? responses[`GET ${fb}/tickets`]).body.items.find((t) => t.owner && t.status.code !== 'closed');
  if (owned) {
    const ap = `${fb}/tickets/${owned.id}/assignment`;
    responses[`GET ${ap}-history`] = await rec('GET', `${ap}-history`);
    const to = members.find((m) => m.userId !== owned.owner.id);
    const req = { ownerUserId: to.userId, version: owned.version, reason: 'Contoh pengalihan untuk paket review (dummy).' };
    responses[`POST ${ap}#stale`] = await rec('POST', ap, { 'X-CSRF-Token': csrf }, { ...req, version: owned.version - 1 });
    responses[`POST ${ap}#ok`] = await rec('POST', ap, { 'X-CSRF-Token': csrf }, req);
    responses[`GET ${ap}-history#after`] = await rec('GET', `${ap}-history`);
  }
}
// Any other workspace path replays the server's real 404 (unknown/foreign/malformed look alike).
responses['GET #not_found'] = await rec('GET', '/api/v1/workspaces/00000000-0000-4000-8000-000000000000');

const fixtures = { recordedAt: new Date().toISOString(), origin: BASE, backendSha: process.env.BACKEND_SHA ?? null, responses };
writeFileSync(OUT, JSON.stringify(fixtures) + '\n');
console.log(`recorded ${Object.keys(responses).length} responses -> ${OUT.pathname}`);
