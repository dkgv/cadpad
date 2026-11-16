import React,{useId,useState} from 'react';
import {Icon} from './ui';

const statusLabels={added:'Added',modified:'Modified',deleted:'Deleted',conflict:'Needs review',other:'Changed'};
type Status=keyof typeof statusLabels;
function fileStatus(state:string,conflict:boolean):Status{return conflict?'conflict':state==='added'||state==='modified'||state==='deleted'?state:state==='removed'?'deleted':'other';}

type Resolution='keep_cadpad'|'use_external';
export function ExternalChangesPanel({changes,conflicts=[],resolutions,reloadFailed=false,busy,onResolve,onReload,onClose}:{changes:Record<string,string>;conflicts?:string[];resolutions:Record<string,Resolution>;busy:boolean;reloadFailed?:boolean;onResolve:(path:string,value:Resolution)=>void;onReload:()=>void;onClose:()=>void}){
  const title=useId();
  const [filter,setFilter]=useState<Status|'all'>('all'),[query,setQuery]=useState('');
  const entries=Object.entries(changes).sort(([a],[b])=>Number(conflicts.includes(b))-Number(conflicts.includes(a))||a.localeCompare(b)),unresolved=conflicts.some(path=>!resolutions[path]);
  const counts=entries.reduce<Partial<Record<Status,number>>>((counts,[path,state])=>{const status=fileStatus(state,conflicts.includes(path));counts[status]=(counts[status]||0)+1;return counts;},{});
  const visible=entries.filter(([path,state])=>(filter==='all'||fileStatus(state,conflicts.includes(path))===filter)&&path.toLowerCase().includes(query.trim().toLowerCase()));
  return <section className="external-changes-panel" aria-labelledby={title}>
    <header><div><h2 id={title}>Files changed</h2><p>{entries.length?`${entries.length} ${entries.length===1?'file has':'files have'} changed on disk.`:'All files are up to date.'}</p></div><button type="button" className="icon-button" aria-label="Close file change review" onClick={onClose}><Icon name="close" size={17}/></button></header>
    <div className="external-review-body"><div className="external-review-summary" aria-label="Change summary"><button type="button" className="external-review-stat external-status-other" aria-pressed={filter==='all'} onClick={()=>setFilter('all')}><strong>{entries.length}</strong><span>All files</span></button>{(Object.keys(statusLabels) as Status[]).filter(status=>counts[status]).map(status=><button type="button" key={status} className={`external-review-stat external-status-${status}`} aria-pressed={filter===status} onClick={()=>setFilter(filter===status?'all':status)}><strong>{counts[status]}</strong><span>{statusLabels[status]}</span></button>)}</div><p>{conflicts.length?'Choose which version to keep for conflicting files.':reloadFailed?'Automatic reload did not complete. You can retry below.':'Disk changes reload automatically when Cadpad is idle.'} Your current model stays available until the new build succeeds.</p>
      <div className="external-review-filters"><input type="search" aria-label="Filter changed files" placeholder="Filter by filename or path…" value={query} onChange={event=>setQuery(event.target.value)}/><span role="status">{visible.length} of {entries.length} files</span>{(filter!=='all'||query)&&<button type="button" onClick={()=>{setFilter('all');setQuery('');}}>Clear filters</button>}</div>
      {!!entries.length&&!visible.length&&<p className="external-review-empty">No files match your filters.</p>}
      <ul aria-label="Changed files">{visible.map(([path,state])=>{
        const conflict=conflicts.includes(path),status=fileStatus(state,conflict),slash=path.lastIndexOf('/');
        return <li key={path} className={`external-status-${status}`}><div className="external-review-file"><span className="external-review-icon"><Icon name="file" size={18}/></span><span className="external-review-path" aria-label={path} title={path}><strong>{path.slice(slash+1)}</strong>{slash>=0&&<small>{path.slice(0,slash+1)}</small>}</span><span className="external-review-badge">{statusLabels[status]}</span></div>{conflict&&<label className="external-review-conflict">Changed in Cadpad and on disk<select aria-label={`Resolve ${path}`} value={resolutions[path]||''} onChange={event=>onResolve(path,event.target.value as Resolution)}><option value="" disabled>Choose a version</option><option value="keep_cadpad">Keep Cadpad changes</option><option value="use_external">Use disk version</option></select></label>}</li>;
      })}</ul>
      {unresolved&&<p className="external-review-hint">Choose a version for each conflicting file before reloading.</p>}
    </div>
    <footer>{(conflicts.length>0||reloadFailed)&&<button type="button" className="primary-button" disabled={busy||unresolved} onClick={onReload}>{conflicts.length?'Apply choices and rebuild':'Retry reload'}</button>}</footer>
  </section>;
}
