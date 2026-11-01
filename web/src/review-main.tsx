import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { createApi } from './api/client';
import { App } from './app/App';
import fixtures from './review/fixtures.json';
import { replayTransport, type Fixtures } from './review/replayTransport';
import './styles/app.css';

const f = fixtures as Fixtures;
const api = createApi(replayTransport(f));
const note = document.getElementById('review-note');
if (note) {
  note.textContent = `Paket review statis — respons API direkam dari backend UAT nyata ${f.recordedAt}` +
    `${f.backendSha ? ` (backend ${f.backendSha.slice(0, 7)})` : ''}; bukan server live.`;
}
createRoot(document.getElementById('root')!).render(<StrictMode><App api={api} /></StrictMode>);
