import { useState } from 'react';
import { Badge } from '../components/Badge';
import { Button } from '../components/Button';
import { DataTable, type Column } from '../components/DataTable';
import { Dialog } from '../components/Dialog';
import { SelectField, TextField } from '../components/Field';
import { PageHeader } from '../components/PageHeader';
import { EmptyState, ErrorState, ForbiddenState, LoadingState, NotFoundState } from '../components/States';

interface Sample { id: string; subject: string; status: string }
const SAMPLE: Sample[] = [
  { id: 'CONTOH-1', subject: 'Contoh baris statis', status: 'Baru' },
  { id: 'CONTOH-2', subject: 'Bukan data dari server', status: 'Menunggu' },
];
const COLUMNS: Column<Sample>[] = [
  { key: 'id', header: 'ID', cell: (r) => r.id },
  { key: 'subject', header: 'Subjek', cell: (r) => r.subject },
  { key: 'status', header: 'Status', cell: (r) => <Badge tone={r.status === 'Baru' ? 'blue' : 'waiting'}>{r.status}</Badge> },
];

/** UAT-only gallery of D04 foundations (C31 states and dialogs), with static sample content. */
export function ComponentsPage() {
  const [dialog, setDialog] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [name, setName] = useState('');
  const [submitted, setSubmitted] = useState(false);
  const nameError = submitted && !name.trim() ? 'Nama wajib diisi.' : undefined;

  return (
    <>
      <PageHeader eyebrow="Khusus Mode uji" title="Komponen dasar"
        description="Fondasi UI D04. Semua isi di halaman ini contoh statis, bukan data atau fungsi produk." />

      <section className="card" aria-labelledby="g-buttons">
        <h2 id="g-buttons">Tombol dan badge</h2>
        <div className="row">
          <Button variant="primary">Utama</Button>
          <Button>Sekunder</Button>
          <Button variant="ghost">Ghost</Button>
          <Button disabled>Nonaktif</Button>
        </div>
        <div className="row" style={{ marginTop: 12 }}>
          <Badge>Netral</Badge><Badge tone="blue">Baru</Badge><Badge tone="waiting">Menunggu</Badge>
          <Badge tone="ok">Selesai</Badge><Badge tone="error">Gagal</Badge>
        </div>
      </section>

      <section className="card" aria-labelledby="g-form">
        <h2 id="g-form">Formulir</h2>
        <form className="formgrid" noValidate onSubmit={(e) => { e.preventDefault(); setSubmitted(true); }}>
          <TextField label="Nama contoh" value={name} onChange={(e) => setName(e.target.value)}
            hint="Validasi hanya di contoh ini." error={nameError} />
          <SelectField label="Prioritas contoh" defaultValue="normal">
            <option value="normal">Normal</option><option value="tinggi">Tinggi</option>
          </SelectField>
          <div className="full row"><Button type="submit" variant="primary">Validasi contoh</Button></div>
        </form>
      </section>

      <section className="card" aria-labelledby="g-table">
        <h2 id="g-table">Tabel</h2>
        <DataTable caption="Tabel contoh (statis)" columns={COLUMNS} rows={SAMPLE} rowKey={(r) => r.id}
          empty={{ title: 'Belum ada data', body: 'Contoh kosong.' }} />
      </section>

      <section className="card" aria-labelledby="g-states">
        <h2 id="g-states">State</h2>
        <div className="gallery">
          <LoadingState label="Contoh memuat…" rows={2} />
          <EmptyState title="Contoh kosong" body="Belum ada item yang cocok." />
          <ErrorState title="Contoh error" body="Permintaan gagal. Coba lagi." requestId="contoh-req-id" onRetry={() => {}} />
          <ForbiddenState />
          <NotFoundState />
        </div>
      </section>

      <section className="card" aria-labelledby="g-dialog">
        <h2 id="g-dialog">Dialog</h2>
        <div className="row">
          <Button onClick={() => setDialog(true)}>Buka dialog</Button>
          <Button onClick={() => setConfirm(true)}>Buka konfirmasi berdampak</Button>
        </div>
      </section>

      <Dialog open={dialog} onClose={() => setDialog(false)} title="Dialog contoh"
        footer={<Button variant="primary" onClick={() => setDialog(false)}>Selesai</Button>}>
        <p>Escape atau tombol tutup menutup dialog; fokus kembali ke tombol pembuka.</p>
      </Dialog>
      <Dialog open={confirm} onClose={() => setConfirm(false)} title="Konfirmasi contoh"
        initialFocus="[data-cancel]"
        footer={<>
          <Button data-cancel onClick={() => setConfirm(false)}>Batal</Button>
          <Button variant="primary" onClick={() => setConfirm(false)}>Lanjutkan contoh</Button>
        </>}>
        <p>Untuk aksi berdampak tinggi, fokus awal ada di Batal. Tidak ada aksi yang dijalankan.</p>
      </Dialog>
    </>
  );
}
