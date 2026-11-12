import React,{useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {Icon,errorSummary} from './ui';

type MeshSummary={path:string;bytes:number;format:string;triangles:number;min:number[];max:number[];size:number[]};
function measure(value:number){return `${Number.isFinite(value)?value.toLocaleString(undefined,{maximumFractionDigits:3}):'—'} mm`;}
function fileSize(bytes:number){
  if(bytes<1024)return `${bytes} B`;
  if(bytes<1024*1024)return `${(bytes/1024).toFixed(1)} KB`;
  return `${(bytes/(1024*1024)).toFixed(1)} MB`;
}

export function MeshInspect({workspace,path,onClose}:{workspace:string;path:string;onClose:()=>void}){
  const dialog=useRef<HTMLDialogElement>(null);
  const [summary,setSummary]=useState<MeshSummary|null>(null);
  const [error,setError]=useState('');
  useEffect(()=>{const node=dialog.current;if(node&&!node.open)node.showModal();return()=>{if(node?.open)node.close();};},[]);
  useEffect(()=>{
    let active=true;
    setSummary(null);setError('');
    invoke<MeshSummary>('inspect_project_mesh',{workspace,path}).then(result=>{if(active)setSummary(result);}).catch(failure=>{if(active)setError(errorSummary(JSON.stringify(failure)));});
    return()=>{active=false;};
  },[workspace,path]);
  const name=path.split('/').at(-1)||path;
  return <dialog ref={dialog} className="source-preview-dialog mesh-inspect" aria-label={`STL: ${path}`} onCancel={event=>{event.preventDefault();onClose();}}>
    <header><strong>{name}</strong><button type="button" className="icon-button" aria-label="Close STL inspection" onClick={onClose}><Icon name="close" size={16}/></button></header>
    {error?<p role="alert">{error}</p>:!summary?<p role="status">Reading mesh…</p>:<dl>
      <dt>Path</dt><dd>{summary.path}</dd>
      <dt>Format</dt><dd>{summary.format==='ascii'?'ASCII STL':'Binary STL'}</dd>
      <dt>Triangles</dt><dd>{summary.triangles.toLocaleString()}</dd>
      <dt>File size</dt><dd>{fileSize(summary.bytes)}</dd>
      <dt>Size</dt><dd>{summary.size.map(measure).join(' × ')}</dd>
      <dt>Minimum</dt><dd>{summary.min.map(measure).join(', ')}</dd>
      <dt>Maximum</dt><dd>{summary.max.map(measure).join(', ')}</dd>
    </dl>}
  </dialog>;
}
