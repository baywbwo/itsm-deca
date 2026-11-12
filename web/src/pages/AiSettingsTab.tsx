import { useCallback, useEffect, useState } from 'react';
import type { Api } from '../api/client';
import type { AiProviderView, AiSettingsView, McpServerView, Workspace } from '../api/types';
const errorMessage = (e: unknown) => (e instanceof Error && e.message ? e.message : 'Permintaan tidak dapat diproses.');

/**
 * D18 — setelan koneksi LLM (OpenAI / Anthropic) dan MCP server.
 *
 * Tidak ada kunci API yang dikirim dari atau ditampilkan di layar ini: yang disimpan hanya NAMA
 * variabel lingkungan, dan server melaporkan apakah variabel itu terisi. Nilai rahasia diisi di
 * server (env / secret manager), bukan di aplikasi.
 */
export function AiSettingsTab({ api, workspace, onNotice }: {
  api: Api;
  workspace: Workspace;
  onNotice: (text: string) => void;
}) {
  const [data, setData] = useState<AiSettingsView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(async (signal?: AbortSignal) => {
    try {
      setData(await api.getAiSettings(workspace.id, signal));
      setError(null);
    } catch (e) {
      if ((e as Error)?.name !== 'AbortError') setError(errorMessage(e));
    }
  }, [api, workspace.id]);

  useEffect(() => {
    const c = new AbortController();
    void load(c.signal);
    return () => c.abort();
  }, [load]);

  async function run(key: string, fn: () => Promise<void>) {
    setBusy(key);
    try {
      await fn();
    } catch (e) {
      onNotice(errorMessage(e));
    } finally {
      setBusy(null);
    }
  }

  if (error) return <p className="field-error" role="alert">{error}</p>;
  if (!data) return <p className="muted">Memuat setelan koneksi…</p>;

  return (
    <div className="ai-settings">
      <p className="muted">
        Kunci API tidak disimpan di aplikasi. Isi variabel lingkungan pada server, lalu jalankan uji koneksi —
        nilai rahasia tidak pernah ditampilkan di layar ini.
      </p>

      <div className="grid-2">
        {data.providers.map((p) => (
          <ProviderCard key={p.provider} api={api} workspace={workspace} provider={p} busy={busy}
            onDone={(msg) => { onNotice(msg); void load(); }} run={run} />
        ))}
      </div>

      <McpSection api={api} workspace={workspace} servers={data.mcpServers} busy={busy}
        onDone={(msg) => { onNotice(msg); void load(); }} run={run} />

      <div className="ai-notes">
        {data.notes.map((n) => <p key={n} className="tiny muted">• {n}</p>)}
      </div>
    </div>
  );
}

function ProviderCard({ api, workspace, provider, busy, onDone, run }: {
  api: Api;
  workspace: Workspace;
  provider: AiProviderView;
  busy: string | null;
  onDone: (msg: string) => void;
  run: (key: string, fn: () => Promise<void>) => Promise<void>;
}) {
  const code = provider.provider as 'openai' | 'anthropic';
  const label = code === 'openai' ? 'OpenAI' : 'Anthropic Claude';
  const [model, setModel] = useState(provider.model ?? (code === 'openai' ? 'gpt-4.1-mini' : 'claude-sonnet-4-5'));
  const [keyEnv, setKeyEnv] = useState(provider.keyEnv ?? (code === 'openai' ? 'OPENAI_API_KEY' : 'ANTHROPIC_API_KEY'));
  const [baseUrl, setBaseUrl] = useState(provider.baseUrl ?? '');
  const key = busy === code || busy === `${code}:test`;

  return (
    <section className="card">
      <header className="card-head">
        <h2>{label}</h2>
        <span className={provider.state === 'tested' ? 'badge badge-ok' : 'badge badge-waiting'}>{provider.state}</span>
      </header>
      <div className="card-body">
        <label htmlFor={`${code}-model`}>Model</label>
        <input id={`${code}-model`} className="field-control" value={model} maxLength={100}
          onChange={(e) => setModel(e.target.value)} disabled={key} />

        <label htmlFor={`${code}-env`}>Nama variabel kunci (server)</label>
        <input id={`${code}-env`} className="field-control" value={keyEnv} maxLength={61}
          onChange={(e) => setKeyEnv(e.target.value.toUpperCase())} disabled={key} />
        <p className="tiny muted">
          {provider.keyPresent ? 'Terpasang di server' : 'Belum terpasang di server'} · nilai tidak pernah dikirim ke browser
        </p>

        <label htmlFor={`${code}-base`}>Base URL (opsional)</label>
        <input id={`${code}-base`} className="field-control" placeholder="https://…" value={baseUrl} maxLength={200}
          onChange={(e) => setBaseUrl(e.target.value)} disabled={key} />

        <div className="row-actions">
          <button className="btn btn-primary" disabled={key}
            onClick={() => void run(code, async () => {
              await api.saveAiProvider(workspace.id, code, {
                model, keyEnv, baseUrl: baseUrl.trim() || null, version: provider.version || null,
              });
              onDone(`${label}: konfigurasi disimpan sebagai draft.`);
            })}>{key ? 'Menyimpan…' : 'Simpan'}</button>
          <button className="btn" disabled={key || provider.version === 0}
            onClick={() => void run(`${code}:test`, async () => {
              const r = await api.testAiProvider(workspace.id, code);
              onDone(`${label}: ${r.lastTestNote ?? 'uji selesai'}`);
            })}>Uji koneksi</button>
        </div>
        {provider.lastTestNote && <p className="tiny muted">Terakhir: {provider.lastTestNote}</p>}
      </div>
    </section>
  );
}

function McpSection({ api, workspace, servers, busy, onDone, run }: {
  api: Api;
  workspace: Workspace;
  servers: McpServerView[];
  busy: string | null;
  onDone: (msg: string) => void;
  run: (key: string, fn: () => Promise<void>) => Promise<void>;
}) {
  const [name, setName] = useState('');
  const [transport, setTransport] = useState<'http' | 'stdio'>('http');
  const [target, setTarget] = useState('');
  const [authEnv, setAuthEnv] = useState('');

  return (
    <section className="card" style={{ marginTop: 16 }}>
      <header className="card-head"><h2>MCP server</h2><span className="badge">{servers.length} terdaftar</span></header>
      <div className="card-body">
        <div className="mcp-form">
          <input className="field-control" placeholder="Nama (mis. lark-base)" value={name} maxLength={120}
            onChange={(e) => setName(e.target.value)} aria-label="Nama MCP server" />
          <select className="field-control" value={transport} aria-label="Transport"
            onChange={(e) => setTransport(e.target.value as 'http' | 'stdio')}>
            <option value="http">HTTP</option>
            <option value="stdio">stdio (dijalankan host agen)</option>
          </select>
          <input className="field-control" placeholder={transport === 'http' ? 'https://…' : 'perintah (mis. npx server)'}
            value={target} maxLength={200} onChange={(e) => setTarget(e.target.value)} aria-label="Target" />
          <input className="field-control" placeholder="Variabel auth (opsional)" value={authEnv} maxLength={61}
            onChange={(e) => setAuthEnv(e.target.value.toUpperCase())} aria-label="Variabel auth" />
          <button className="btn btn-primary" disabled={!name.trim() || !target.trim() || busy === 'mcp:new'}
            onClick={() => void run('mcp:new', async () => {
              await api.createMcpServer(workspace.id, {
                name: name.trim(), transport,
                endpoint: transport === 'http' ? target.trim() : null,
                command: transport === 'stdio' ? target.trim() : null,
                authEnv: authEnv.trim() || null,
              });
              setName(''); setTarget(''); setAuthEnv('');
              onDone('MCP server ditambahkan.');
            })}>Tambah</button>
        </div>

        {servers.length === 0
          ? <p className="muted">Belum ada MCP server terdaftar.</p>
          : (
            <table className="table">
              <thead><tr><th>Nama</th><th>Transport</th><th>Target</th><th>Auth</th><th>Aktif</th><th>Uji</th></tr></thead>
              <tbody>
                {servers.map((s) => (
                  <tr key={s.id}>
                    <td>{s.name}</td>
                    <td>{s.transport}</td>
                    <td className="tiny">{s.transport === 'http' ? s.endpoint : s.command}</td>
                    <td className="tiny">{s.authEnv ? `${s.authEnv} · ${s.authPresent ? 'terpasang' : 'belum'}` : '—'}</td>
                    <td>
                      <button className="btn btn-small" disabled={busy === s.id}
                        onClick={() => void run(s.id, async () => {
                          await api.updateMcpServer(workspace.id, s.id, { enabled: !s.enabled, version: s.version });
                          onDone(`${s.name}: ${s.enabled ? 'dinonaktifkan' : 'diaktifkan'}.`);
                        })}>{s.enabled ? 'Nonaktifkan' : 'Aktifkan'}</button>
                    </td>
                    <td>
                      <button className="btn btn-small" disabled={busy === `${s.id}:test`}
                        onClick={() => void run(`${s.id}:test`, async () => {
                          const r = await api.testMcpServer(workspace.id, s.id);
                          onDone(`${r.name}: ${r.lastTestNote ?? 'uji selesai'}`);
                        })}>Uji</button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
      </div>
    </section>
  );
}
