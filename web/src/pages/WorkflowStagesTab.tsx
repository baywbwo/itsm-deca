import { useId, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type { WorkflowStage, Workspace } from '../api/types';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { DataTable } from '../components/DataTable';
import { useLoad } from './adminKit';

const lifecycle = {new:'Tiket baru',open:'Sedang ditangani',pending_customer:'Menunggu pelanggan',pending_engineering:'Menunggu pekerjaan eksternal',resolved:'Selesai',closed:'Arsip'};
export function WorkflowStagesTab({api,workspace}: {api:Api;workspace:Workspace}) {
 const state=useLoad(signal=>api.getWorkflowStages(workspace.id,signal),workspace.id);
 const [editing,setEditing]=useState<WorkflowStage|null>(null);
 const [busy,setBusy]=useState(false);
 const [error,setError]=useState('');
 const id=useId();
 function edit(stage:WorkflowStage){setEditing({...stage});setError('');}
 async function save(){
  if(!editing||busy||!editing.label.trim())return;
  setBusy(true);setError('');
  try{await api.saveWorkflowStage(workspace.id,editing);setEditing(null);state.reload();}
  catch(e){setError(e instanceof ApiError && e.code==='stage_in_use'?'Pindahkan tiket dari stage ini terlebih dahulu sebelum menonaktifkannya.':
   e instanceof ApiError && e.code==='required_stage'?'Sediakan minimal satu stage aktif untuk tiket baru, penanganan, dan selesai.':
   e instanceof Error?e.message:'Pengaturan belum tersimpan.');}
  finally{setBusy(false);}
 }
 return <section className="set-sec"><div className="bd">
  <h2>Status &amp; stage tiket</h2>
  <p className="hintline spaced-bottom">Atur nama dan urutan kolom board. Fungsi stage menentukan alur penanganan, ringkasan penyelesaian, dan pembukaan kembali tiket.</p>
  {state.error!=null&&<p role="alert">Pengaturan gagal dimuat. <Button onClick={state.reload}>Coba lagi</Button></p>}
  {!state.data&&!state.error&&<p>Memuat stage…</p>}
  {state.data&&<>
   {state.data.canEdit&&<div className="row spaced-bottom"><Button onClick={()=>edit({code:'stage_'+crypto.randomUUID().replaceAll('-',''),label:'',lifecycle:'open',sortOrder:Math.min(10000,Math.max(0,...state.data!.items.map(x=>x.sortOrder))+10),active:true,version:0})}>Tambah stage</Button></div>}
   <DataTable caption="Stage tiket perusahaan" rows={state.data.items} rowKey={stage=>stage.code}
     empty={{title:'Belum ada stage',body:'Tambahkan stage untuk alur tiket.'}}
     columns={[
       {key:'label',header:'Nama stage',cell:stage=>stage.label},
       {key:'lifecycle',header:'Fungsi',cell:stage=>lifecycle[stage.lifecycle as keyof typeof lifecycle]??stage.lifecycle},
       {key:'order',header:'Urutan',cell:stage=>stage.sortOrder},
       {key:'active',header:'Status',cell:stage=>stage.active?'Aktif':'Nonaktif'},
       {key:'edit',header:'Tindakan',cell:stage=>state.data!.canEdit&&<Button onClick={()=>edit(stage)}>Edit {stage.label}</Button>},
     ]}/>

  </>}
  {editing&&<Dialog open title={editing.version?'Edit stage':'Tambah stage'} onClose={()=>{if(!busy)setEditing(null);}}
   footer={<><Button disabled={busy} onClick={()=>setEditing(null)}>Batal</Button><Button variant="primary" disabled={busy||!editing.label.trim()} onClick={()=>void save()}>{busy?'Menyimpan…':'Simpan stage'}</Button></>}>
   <form className="formgrid" onSubmit={e=>{e.preventDefault();void save();}}>
    <div className="field full"><label htmlFor={id+'name'}>Nama stage</label><input id={id+'name'} className="field-control" required maxLength={100} value={editing.label} disabled={busy} onChange={e=>setEditing({...editing,label:e.target.value})}/></div>
    <div className="field"><label htmlFor={id+'type'}>Fungsi stage</label><select id={id+'type'} className="field-control" disabled={busy||editing.version>0} value={editing.lifecycle} onChange={e=>setEditing({...editing,lifecycle:e.target.value})}>{Object.entries(lifecycle).filter(([code])=>code!=='closed'||editing.lifecycle==='closed').map(([code,label])=><option key={code} value={code}>{label}</option>)}</select></div>
    <div className="field"><label htmlFor={id+'order'}>Urutan kolom</label><input id={id+'order'} className="field-control" type="number" min={0} max={10000} value={editing.sortOrder} disabled={busy} onChange={e=>setEditing({...editing,sortOrder:Number(e.target.value)})}/></div>
    <label><input type="checkbox" disabled={busy} checked={editing.active} onChange={e=>setEditing({...editing,active:e.target.checked})}/> Stage aktif</label>
    {error&&<p role="alert" className="full">{error}</p>}
   </form>
  </Dialog>}
 </div></section>;
}
