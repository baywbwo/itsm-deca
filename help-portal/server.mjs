import http from 'node:http';
import { readFile } from 'node:fs/promises';

import { notifications } from './notifications.mjs';

process.umask(0o077);
const html = await readFile(new URL('./deca-help.html', import.meta.url));
const config = process.env;
const intakeReady = ['ITSM_USERNAME', 'ITSM_PASSWORD', 'ITSM_WORKSPACE_ID', 'ITSM_CHANNEL_ID']
  .every(name => Boolean(config[name]));
const setupMessage = 'Pengiriman tiket belum aktif. Untuk menghubungi CS, gunakan Chat via WhatsApp.';
const origin = new URL(config.ITSM_ORIGIN || 'https://itsm.scowork.my.id:8443');
if (origin.protocol !== 'https:') throw new Error('ITSM requires HTTPS');
const ticketPath = '/api/v1/workspaces/' + encodeURIComponent(config.ITSM_WORKSPACE_ID) + '/tickets';
const categories = new Set(['QRIS', 'Settlement', 'Top up', 'Akun & Verifikasi', 'Refund', 'Lainnya']);

class InvalidForm extends Error {}

function ticketBody(raw) {
  function field(name, limit, optional = false) {
    const value = raw?.[name];
    if (typeof value !== 'string' || value.length > limit || (!optional && !value.trim())) {
      throw new InvalidForm();
    }
    return value.trim();
  }
  const merchant = field('merchant', 120), name = field('name', 120);
  const email = field('email', 254), wa = field('wa', 32, true);
  const category = field('category', 40), detail = field('body', 4000);
  const receivedAt = field('receivedAt', 32);
  if (!categories.has(category) || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) ||
      !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/.test(receivedAt) || !Number.isFinite(Date.parse(receivedAt))) {
    throw new InvalidForm();
  }
  return {subject: category + ' — ' + merchant, receivedAt, intakeChannelId: config.ITSM_CHANNEL_ID,
    chronology: ['Merchant: ' + merchant, 'PIC: ' + name, 'Email: ' + email,
      ...(wa ? ['WhatsApp: ' + wa] : []), 'Kategori: ' + category, '', detail].join('\n')};
}

function post(path, body, headers = {}) {
  return fetch(new URL(path, origin), {method: 'POST', redirect: 'error', signal: AbortSignal.timeout(15000),
    headers: {'Content-Type': 'application/json', ...headers}, body: JSON.stringify(body)});
}

async function login() {
  const response = await post('/api/v1/login', {username: config.ITSM_USERNAME, password: config.ITSM_PASSWORD}, {'X-ITSM-Login': '1'});
  if (!response.ok) throw new Error('ITSM login HTTP ' + response.status);
  const data = await response.json();
  const cookie = response.headers.getSetCookie().map(line => line.split(';')[0])
    .find(value => value.startsWith('__Host-itsm_session='));
  if (!cookie || !data.csrfToken || data.mustChangePassword || data.mfaRequired && !data.mfaEnrolled) {
    throw new Error('ITSM service session unavailable');
  }
  return {Cookie: cookie, 'X-CSRF-Token': data.csrfToken};
}

let session;
async function itsmApi(path, body, extraHeaders = {}) {
  for (let attempt = 0; attempt < 2; attempt++) {
    const current = session ??= login().catch(error => { session = undefined; throw error; });
    const headers = {...await current, ...extraHeaders};
    const response = body ? await post(path, body, headers) : await fetch(new URL(path, origin),
      {headers, redirect: 'error', signal: AbortSignal.timeout(15000)});
    if (response.status === 401 && attempt === 0) {
      await response.body?.cancel();
      if (session === current) session = undefined;
      continue;
    }
    if (!response.ok) throw new Error('ITSM HTTP ' + response.status);
    return response.json();
  }
}
async function createTicket(body,key) {
  const {ticket} = await itsmApi(ticketPath, body, {'Idempotency-Key': key});
  if (!ticket?.id || !ticket.displayNumber) throw new Error('ITSM response missing ticket');
  return ticket;
}
const mail = notifications(itsmApi,ticketPath);
if (intakeReady) mail.start(createTicket);

http.createServer(async (req, res) => {
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('X-Content-Type-Options', 'nosniff');
  function json(status, data) {
    res.writeHead(status, {'Content-Type': 'application/json; charset=utf-8'});
    res.end(JSON.stringify(data));
  }
  if (req.method === 'GET' && ['/help/', '/help', '/help/deca-help.html'].includes(req.url)) {
    res.writeHead(200, {'Content-Type': 'text/html; charset=utf-8'});
    return res.end(intakeReady ? html : html.toString().replace('<form id="f">',
      '<form id="f"><p role="status">' + setupMessage + '</p>'));
  }
  if (req.url !== '/help/api/itsm-tickets') return json(404, {});
  if (req.method !== 'POST') return json(405, {});
  if (!intakeReady) return json(503, {code: 'intake_unconfigured', error: setupMessage});
  if (req.headers['content-type']?.split(';')[0] !== 'application/json') return json(415, {});
  const key = req.headers['idempotency-key'];
  if (!/^[a-f0-9]{32}$/.test(key || '')) return json(400, {error: 'Muat ulang halaman formulir.'});
  try {
    const chunks = [];
    let size = 0;
    for await (const chunk of req) {
      size += chunk.length;
      if (size > 32768) return json(413, {error: 'Laporan terlalu panjang.'});
      chunks.push(chunk);
    }
    let raw;
    try { raw = JSON.parse(Buffer.concat(chunks).toString()); } catch { throw new InvalidForm(); }
    const body = ticketBody(raw);
    const email = raw.email.trim();
    const intakeKey = 'help-' + key;
    mail.remember(intakeKey, body, email);
    const ticket = await createTicket(body, intakeKey);
    mail.register(ticket,email,intakeKey);
    json(200, {ticket:ticket.displayNumber});
  } catch (error) {
    if (error instanceof InvalidForm) return json(422, {error: 'Periksa data pelapor, email, dan isi kendala.'});
    console.error('Portal intake:', error.message);
    json(503, {error: 'ITSM belum mengonfirmasi pengiriman. Coba lagi dengan isian yang sama.'});
  }
}).listen(Number(config.PORT || 3100), config.PORTAL_HOST || '127.0.0.1');
