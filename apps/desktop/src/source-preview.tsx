import React,{useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {Icon,errorSummary} from './ui';

export function SourcePreview({workspace,revision,path,onClose}:{workspace:string;revision:string;path:string;onClose:()=>void}){
  const dialog=useRef<HTMLDialogElement>(null),[source,setSource]=useState<string|null>(null),[error,setError]=useState('');
  useEffect(()=>{const node=dialog.current;if(node&&!node.open)node.showModal();return()=>{if(node?.open)node.close();};},[]);
  useEffect(()=>{let active=true;setSource(null);setError('');invoke<{source:string}>('read_source',{workspace,version:revision,path}).then(result=>{if(active)setSource(result.source);}).catch(error=>{if(active)setError(errorSummary(JSON.stringify(error)));});return()=>{active=false;};},[workspace,revision,path]);
  return <dialog ref={dialog} className="source-preview-dialog" aria-label={`Source: ${path}`} onCancel={event=>{event.preventDefault();onClose();}}><header><strong>{path}</strong><button type="button" className="icon-button" aria-label="Close source preview" onClick={onClose}><Icon name="close" size={16}/></button></header>{error?<p role="alert">{error}</p>:source===null?<p role="status">Loading source…</p>:<pre tabIndex={0} aria-label="Source code">{source}</pre>}</dialog>;
}
