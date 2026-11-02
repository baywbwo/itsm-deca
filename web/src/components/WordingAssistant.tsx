import { useState } from 'react';
import type { Api } from '../api/client';
import { ApiError } from '../api/client';
import { Button } from './Button';

/** Suggestions are reviewed before application; applying never submits a form. */
export function WordingAssistant({ api, workspaceId, text, mode, onApply, disabled = false }: {
  api: Api; workspaceId: string; text: string; mode: 'chronology' | 'reply'; onApply: (text: string) => void; disabled?: boolean;
}) {
  const [busy, B] = useState(false), [error, E] = useState('');
  const [result, R] = useState<{ source: string; suggestion: string } | null>(null);
  async function suggest() {
    if (busy || disabled || !text.trim() || text.length > 6000) return;
    const source = text;
    B(true); E(''); R(null);
    try { const r = await api.suggestWording(workspaceId, mode, source); R({ source, suggestion: r.suggestion }); }
    catch (e) { E(e instanceof ApiError && e.status === 503 ? 'GPT belum tersedia. Periksa konfigurasi API dan model pada server.' : 'Saran belum dapat dibuat. Teks Anda tetap tersimpan di formulir.'); }
    finally { B(false); }
  }
  return <div className="wording-assistant">
    <div className="row between"><strong>✦ Asisten penulisan GPT</strong>
      <Button type="button" disabled={busy || disabled || !text.trim() || text.length > 6000} onClick={() => void suggest()} aria-busy={busy}>
        {busy ? 'Menyusun saran…' : mode === 'chronology' ? 'Rapikan kronologi' : 'Buat draf balasan'}
      </Button>
    </div>
    <p className="tiny muted">Hanya teks isian ini yang dikirim ke OpenAI. Hapus nama, PIN, OTP, password, dan informasi sensitif terlebih dahulu. Maksimal 6.000 karakter.</p>
    {error && <p role="alert" className="field-error">{error}</p>}
    {result && <div className="wording-result" role="status"><p>{result.suggestion}</p>
      <p className="tiny muted">Periksa fakta dan placeholder data sebelum digunakan. Saran tidak dikirim otomatis kepada pelanggan.</p>
      {result.source !== text && <p className="tiny">Teks sumber sudah berubah. Buat saran baru agar perubahan Anda tetap terjaga.</p>}
      <div className="row"><Button type="button" variant="primary" disabled={disabled || result.source !== text} onClick={() => { onApply(result.suggestion); R(null); }}>Gunakan saran</Button>
      <Button type="button" onClick={() => R(null)}>Abaikan</Button></div>
    </div>}
  </div>;
}
