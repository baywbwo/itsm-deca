import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { createApi, fetchTransport } from './api/client';
import { App } from './app/App';
import './styles/app.css';
import './styles/workspace.css';

const api = createApi(fetchTransport);
createRoot(document.getElementById('root')!).render(<StrictMode><App api={api} /></StrictMode>);
