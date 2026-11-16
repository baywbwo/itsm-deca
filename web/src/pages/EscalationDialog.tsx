import { useId, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type { TicketDetail, Workspace } from '../api/types';
import { emptyForm, toRequest, validateIntake } from '../app/drafts';
import { instantLabel } from '../app/format';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { AssignmentFields } from './IntakeDialog';
import { useLoad } from './adminKit';

export function EscalationDialog({api, workspace, detail, onClose, onSaved}: {
  api: Api; workspace: Workspace; detail: TicketDetail; onClose: () => void; onSaved: () => void;
}) {
  const directory = useLoad(signal => api.getIntakeDirectory(workspace.id, signal), workspace.id);
  const id = useId();
  const [form, setForm] = useState(() => emptyForm(''));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const {ticket, properties} = detail;
  const errors = validateIntake(form).filter(e => e.field.startsWith('jira') || e.field === 'larkDestinationId');
  async function save() {
    if (busy || errors.length || (!form.larkDestinationId && form.jiraMode === 'none')) return;
    setBusy(true);
    setError('');
    try {
      const {jira, larkDestinationId} = toRequest(form, workspace.timezone);
      await api.escalateTicket(workspace.id, ticket.id, {version: ticket.version, jira: jira ?? null, larkDestinationId: larkDestinationId ?? null});
      onSaved();
    } catch (e) {
      setError(e instanceof ApiError && e.status === 409 ? 'Tiket telah berubah. Tutup dialog dan muat ulang detail tiket.' :
        e instanceof Error ? e.message : 'Eskalasi belum terkonfirmasi.');
      setBusy(false);
    }
  }
  return <Dialog open title={`Eskalasi · ${ticket.displayNumber}`} onClose={() => { if (!busy) onClose(); }}
    footer={<><Button onClick={onClose} disabled={busy}>Batal</Button><Button variant="primary" disabled={busy || !directory.data || !!errors.length || (form.jiraMode === 'none' && !form.larkDestinationId)} onClick={() => void save()}>{busy ? 'Menyimpan…' : 'Simpan eskalasi'}</Button></>}>
    <details className="intake-operations full">
      <summary>Data penerimaan &amp; penanganan <span>Waktu diterima, PIC, unit, wilayah, dan direktori merchant</span></summary>
      <dl className="kv"><dt>Diterima</dt><dd>{instantLabel(properties.receivedAt,workspace.timezone)}</dd>
        <dt>PIC</dt><dd>{ticket.owner?.label ?? 'Belum ditugaskan'}</dd>
        <dt>Unit</dt><dd>{properties.receivingUnit?.label ?? '—'}</dd><dt>Wilayah</dt><dd>{properties.region?.label ?? '—'}</dd>
        <dt>Merchant</dt><dd>{properties.merchant?.label ?? '—'}</dd></dl>
    </details>
    {directory.data ? <AssignmentFields api={api} workspace={workspace} directory={directory.data} id={id}
      f={form} set={(key,value) => setForm(prev => ({...prev,[key]:value}))} locked={busy} expanded
      errorOf={key => errors.find(e => e.field === key)?.message} /> :
      directory.error ? <p role="alert">Direktori integrasi gagal dimuat. <Button onClick={directory.reload}>Coba lagi</Button></p> : <p>Memuat Jira dan Lark…</p>}
    {error && <p role="alert">{error}</p>}
  </Dialog>;
}
