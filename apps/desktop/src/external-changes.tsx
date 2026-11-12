import React,{useEffect,useId,useRef} from 'react';
import {Icon} from './ui';

type Resolution='keep_cadpad'|'use_external';
export function ExternalChangesDialog({changes,conflicts=[],resolutions,busy,onResolve,onReload,onClose}:{changes:Record<string,string>;conflicts?:string[];resolutions:Record<string,Resolution>;busy:boolean;onResolve:(path:string,value:Resolution)=>void;onReload:()=>void;onClose:()=>void}){
  const dialog=useRef<HTMLDialogElement>(null),title=useId();
  useEffect(()=>{const node=dialog.current;if(!node)return;node.showModal();return()=>{if(node.open)node.close();};},[]);
  const entries=Object.entries(changes),unresolved=conflicts.some(path=>!resolutions[path]);
  return <dialog ref={dialog} className="external-review-dialog" aria-labelledby={title} onCancel={event=>{event.preventDefault();onClose();}}>
    <header><div><h2 id={title}>Files changed outside Cadpad</h2><p>{entries.length} {entries.length===1?'file has':'files have'} changed on disk.</p></div><button type="button" className="icon-button" aria-label="Close file change review" onClick={onClose}><Icon name="close" size={17}/></button></header>
    <div className="external-review-body"><p>Reload to build the model from these changes. Your current model stays available until the new build succeeds.</p>
      <ul aria-label="Changed files">{entries.map(([path,state])=><li key={path}><div className="external-review-file"><Icon name="file" size={17}/><span>{path}</span><small>{state}</small></div>{conflicts.includes(path)&&<label className="external-review-conflict">Changed in Cadpad and on disk<select aria-label={`Resolve ${path}`} value={resolutions[path]||''} onChange={event=>onResolve(path,event.target.value as Resolution)}><option value="" disabled>Choose a version</option><option value="keep_cadpad">Keep Cadpad changes</option><option value="use_external">Use disk version</option></select></label>}</li>)}</ul>
      {unresolved&&<p className="external-review-hint">Choose a version for each conflicting file before reloading.</p>}
    </div>
    <footer><button type="button" onClick={onClose}>Keep working</button><button type="button" className="primary-button" disabled={busy||unresolved} onClick={onReload}>Reload and build</button></footer>
  </dialog>;
}
