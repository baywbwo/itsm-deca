import { DatabaseSync } from 'node:sqlite';
import { readFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export function notifications(api, ticketsPath) {
  const db = new DatabaseSync(process.env.PORTAL_DB || fileURLToPath(new URL('./notifications.sqlite', import.meta.url)));
  db.exec(`PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS contacts(id TEXT PRIMARY KEY, number TEXT NOT NULL, email TEXT NOT NULL, last_event TEXT);
    CREATE TABLE IF NOT EXISTS messages(seq INTEGER PRIMARY KEY, event TEXT UNIQUE NOT NULL, ticket TEXT NOT NULL,
      body TEXT NOT NULL, sent INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0, retry_at INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS pending(key TEXT PRIMARY KEY, payload TEXT NOT NULL, email TEXT NOT NULL, created INTEGER NOT NULL);
  `);
  function transaction(fn) {
    db.exec('BEGIN IMMEDIATE');
    try { fn(); db.exec('COMMIT'); } catch (e) { db.exec('ROLLBACK'); throw e; }
  }
  function enqueue(id, event, body) {
    db.prepare('INSERT OR IGNORE INTO messages(event,ticket,body) VALUES(?,?,?)').run(event,id,body);
  }
  function register(ticket, email, key) {
    transaction(() => {
      db.prepare('INSERT OR IGNORE INTO contacts(id,number,email) VALUES(?,?,?)').run(ticket.id,ticket.displayNumber,email);
      enqueue(ticket.id, 'receipt-' + ticket.id, 'Pengaduan Anda sudah kami terima. Tim CS DECA akan menindaklanjuti tiket ini.');
      if (key) db.prepare('DELETE FROM pending WHERE key=?').run(key);
    });
  }
  async function sync(contact) {
    const events = [];
    let cursor;
    do {
      const page = await api(ticketsPath + '/' + contact.id + '/activity?limit=100' +
        (cursor ? '&cursor=' + encodeURIComponent(cursor) : ''));
      const index = page.items.findIndex(e => e.id === contact.last_event);
      events.push(...(index < 0 ? page.items : page.items.slice(0,index)));
      cursor = index < 0 && page.nextCursor;
    } while (cursor);
    if (!events.length) return;
    transaction(() => {
      for (const e of events.slice().reverse()) {
        if (e.action === 'ticket.transition') enqueue(contact.id,e.id,'Pembaruan tiket Anda: ' + e.label + '.');
      }
      db.prepare('UPDATE contacts SET last_event=? WHERE id=?').run(events[0].id,contact.id);
    });
  }
  async function deliver() {
    if (!process.env.SMTP_HOST) return;
    if (process.env.SMTP_USERNAME && !process.env.SMTP_PASSWORD &&
        !(await readFile(process.env.SMTP_PASSWORD_FILE, 'utf8').catch(() => '')).trim()) return;
    const rows = db.prepare(`SELECT m.*, c.email, c.number FROM messages m JOIN contacts c ON c.id=m.ticket
      WHERE m.sent=0 AND m.retry_at<=unixepoch() AND NOT EXISTS
      (SELECT 1 FROM messages older WHERE older.ticket=m.ticket AND older.seq<m.seq AND older.sent=0)
      ORDER BY m.seq LIMIT 30`).all();
    if (!rows.length) return;
    try {
      const settings = await api(ticketsPath.replace(/\/tickets$/, '/customer-email'));
      if (settings.paused !== false) return;
    } catch (error) {
      // The live API predates workspace email settings. Only the local installation opts in.
      if (error.message !== 'ITSM HTTP 404' || process.env.EMAIL_LEGACY_API !== '1') return;
    }
    for (const row of rows) {
      try {
        await new Promise((resolve,reject) => {
          const child = spawn(process.env.PYTHON || '/usr/bin/python3',
            [fileURLToPath(new URL('./send_email.py',import.meta.url))], {stdio:['pipe','ignore','inherit'], timeout:60000});
          child.on('error',reject);
          child.on('close',code => code === 0 ? resolve() : reject(new Error('SMTP delivery failed')));
          child.stdin.on('error',reject);
          child.stdin.end(JSON.stringify(row));
        });
        db.prepare('UPDATE messages SET sent=1 WHERE seq=?').run(row.seq);
      } catch {
        const delay = Math.min(3600, 30 * 2 ** Math.min(row.attempts,7));
        db.prepare('UPDATE messages SET attempts=attempts+1,retry_at=unixepoch()+? WHERE seq=?').run(delay,row.seq);
      }
    }
  }
  function start(create) {
    async function poll() {
      try {
        for (const r of db.prepare('SELECT * FROM pending WHERE created < unixepoch()-60 ORDER BY created LIMIT 25').all()) {
          try { register(await create(JSON.parse(r.payload), r.key), r.email, r.key); }
          catch (e) { console.error('Retry intake:',e.message); }
        }
        for (const c of db.prepare('SELECT * FROM contacts').all()) {
          try { await sync(c); } catch (e) { console.error('Status email sync:',e.message); }
        }
      } finally { setTimeout(poll,30000).unref(); }
    }
    async function send() {
      try { await deliver(); } finally { setTimeout(send,5000).unref(); }
    }
    poll(); send();
  }
  function remember(key,body,email) {
    const payload = JSON.stringify(body);
    db.prepare('INSERT OR IGNORE INTO pending VALUES(?,?,?,unixepoch())').run(key,payload,email);
    const row = db.prepare('SELECT payload,email FROM pending WHERE key=?').get(key);
    if (row.payload !== payload || row.email !== email) throw new Error('Intake retry data mismatch');
  }
  return {register,remember,start};
}
