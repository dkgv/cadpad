import React,{useEffect,useRef,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {Icon,errorSummary} from './ui';
import {SourceCatalog} from './source-catalog';

export type EntrySelection={request_id:string;folder:string;entries:string[];current_entry?:string|null};
type EntryPlan={direct:boolean;has_main:boolean;recommended:string|null;source_hash:string;diagnostic?:string|null;options:{id:string;label:string;available:boolean;reason:string;source:string|null}[]};
export function ProjectEntry({selection,busy,error,onOpen,onCancel}:{selection:EntrySelection;busy:boolean;error:string;onOpen:(entry:string,adapter?:string)=>void;onCancel:()=>void}){
  const dialog=useRef<HTMLDialogElement>(null),manual=useRef(false),requested=useRef('');
  const [query,setQuery]=useState('');
  const [entry,setEntry]=useState(()=>selection.current_entry||selection.entries.find(path=>path==='main.py')||selection.entries.find(path=>path==='model.py')||selection.entries[0]||'');
  const [plan,setPlan]=useState<EntryPlan|null>(null),[loading,setLoading]=useState(false),[analysisError,setAnalysisError]=useState(''),[attempt,setAttempt]=useState(0);
  const [mode,setMode]=useState<'existing'|'adapter'>('existing'),[builder,setBuilder]=useState('');
  useEffect(()=>{if(!dialog.current?.open)dialog.current?.showModal();},[]);
  useEffect(()=>{
    if(!entry)return;let active=true;setLoading(true);setPlan(null);setAnalysisError('');setBuilder('');manual.current=false;
    invoke<EntryPlan>('project_entry_plan',{requestId:selection.request_id,entry,builder:requested.current||null}).then(result=>{
      if(!active||!Array.isArray(result.options))return;
      setPlan(result);setBuilder(requested.current||result.recommended||result.options.find(option=>option.available)?.id||'');
      if(!manual.current)setMode(requested.current||!result.direct&&!!result.recommended?'adapter':'existing');
    }).catch(error=>{if(active)setAnalysisError(errorSummary(JSON.stringify(error)));}).finally(()=>{if(active)setLoading(false);});
    return()=>{active=false;};
  },[entry,selection.request_id,attempt]);
  const entries=selection.entries.filter(path=>path.toLowerCase().includes(query.toLowerCase()));
  const selected=plan?.options.find(option=>option.id===builder&&option.available);
  const available=plan?.options.some(option=>option.available);
  return <dialog ref={dialog} className="project-entry-dialog" aria-labelledby="project-entry-title" onCancel={event=>{event.preventDefault();if(!busy)onCancel();}}>
    <form onSubmit={event=>{event.preventDefault();if(entry&&!busy&&(mode==='existing'||selected))onOpen(entry,mode==='adapter'?builder:undefined);}}>
      <header><div><h2 id="project-entry-title">Set up entry point</h2><span className="project-entry-folder"><Icon name="folder" size={15}/>{selection.folder}</span></div><button type="button" className="icon-button" aria-label="Cancel opening project" disabled={busy} onClick={onCancel}><Icon name="close"/></button></header>
      <details className="entry-catalog" open><summary>Browse project builders</summary><SourceCatalog requestId={selection.request_id} disabled={busy} onChoose={(path,builder)=>{requested.current=builder;if(path===entry){setBuilder(builder);setMode('adapter');setAttempt(value=>value+1);}else setEntry(path);}}/></details>
      <p>Choose the Python source for this model.</p>
      {selection.entries.length>6&&<input className="project-entry-search" type="search" aria-label="Find Python file" placeholder="Find a Python file…" value={query} onChange={event=>setQuery(event.target.value)} disabled={busy}/>}
      <fieldset className="project-entry-list" disabled={busy}><legend>Python files</legend>{entries.map(path=><label key={path} className={path===entry?'selected':''}><input type="radio" name="entry-point" value={path} checked={path===entry} onChange={()=>{requested.current='';setEntry(path);}}/><code>{path}</code></label>)}{!entries.length&&<p className="muted">No matching files.</p>}</fieldset>
      {loading&&<p className="entry-analysis" role="status">Finding geometry builders…</p>}
      {plan&&<fieldset className="entry-method" disabled={busy}><legend>How to open it</legend>
        <label><input type="radio" name="entry-method" checked={mode==='adapter'} disabled={!available} onChange={()=>{manual.current=true;setMode('adapter');}}/><span><strong>Create a Cadpad entry point</strong><small>Call a geometry builder and display its result. Skip the script’s main export routine.</small></span></label>
        {mode==='adapter'&&<div className="entry-adapter"><label>Geometry builder<select aria-label="Geometry builder" value={builder} onChange={event=>setBuilder(event.target.value)}>{plan.options.map(option=><option key={option.id} value={option.id} disabled={!option.available}>{option.label}{!option.available?` — ${option.reason}`:''}</option>)}</select></label><p>A new Python file will be created. Your existing source files stay unchanged.</p></div>}
        <label><input type="radio" name="entry-method" checked={mode==='existing'} onChange={()=>{manual.current=true;setMode('existing');}}/><span><strong>Run this file directly</strong><small>Use a file that already displays geometry with show_object(), show(), or result.</small></span></label>
        {!available&&<p className="entry-analysis">{plan.diagnostic||'No builder with default arguments was found. Choose another source file, or use an existing entry point.'}</p>}
      </fieldset>}
      {analysisError&&<p className="entry-analysis">{analysisError} <button type="button" onClick={()=>setAttempt(value=>value+1)}>Try again</button></p>}
      {error&&<p className="project-entry-error" role="alert">{error}</p>}
      <footer><button type="button" onClick={onCancel} disabled={busy}>Cancel</button><button className="primary" disabled={!entries.includes(entry)||busy||mode==='adapter'&&(!selected||loading)} type="submit">{busy?'Opening…':mode==='adapter'?'Create entry point':'Open project'}</button></footer>
    </form>
  </dialog>;
}
