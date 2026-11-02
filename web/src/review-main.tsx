import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { createApi } from './api/client';
import { App } from './app/App';
import fixtures from './review/fixtures.json';
import { replayTransport, SCENARIOS, type Fixtures, type Scenario } from './review/replayTransport';
import './styles/app.css';

const f = fixtures as Fixtures;
const asked = new URLSearchParams(location.search).get('scenario') ?? 'none';
const scenario: Scenario = (SCENARIOS as readonly string[]).includes(asked) ? (asked as Scenario) : 'none';
const api = createApi(replayTransport(f, 250, scenario));
const note = document.getElementById('review-note');
if (note) {
  note.textContent = `Paket review statis — respons API direkam dari backend UAT nyata ${f.recordedAt}` +
    `${f.backendSha ? ` (backend ${f.backendSha.slice(0, 7)})` : ''}; bukan server live. Saved view = simulasi di memori halaman (hilang saat reload), bukan API live. Intake/PIC memutar ulang jawaban nyata yang direkam; Jira/Lark = simulasi berlabel, bukan penyedia nyata. Unggahan memutar ulang penolakan fail-closed nyata (pemindai tidak tersedia di host ini); unggah bersih/unduh hanya dibuktikan tes cargo.` +
    (scenario === 'none' ? '' : ` Skenario simulasi review: ${scenario} (respons disintesis/dipilih dari rekaman, bukan server live).`);
}
createRoot(document.getElementById('root')!).render(<StrictMode><App api={api} /></StrictMode>);
