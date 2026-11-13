import React,{useEffect,useRef,useState,useSyncExternalStore} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {Icon,errorSummary} from './ui';
import {SourcePreview} from './source-preview';

export type Builder={id:string;label:string;available:boolean;reason:string;line:number;kind?:'geometry_candidate'|'part'|'assembly'|'function'};
export type SourceFile={path:string;description:string;builders:Builder[];diagnostic:string|null;recommended?:string|null};
export type IndexedModel={file:SourceFile;builder:Builder};
const indexes=new Map<string,Promise<{files:SourceFile[]}>>();
const locations=new Map<string,string>();
const listeners=new Set<()=>void>();
export function resetSourceCatalogCache(){indexes.clear();locations.clear();for(const listener of listeners)listener();}
export function loadSourceIndex(requestId?:string,workspace?:string,revision?:string){
  const key=JSON.stringify([requestId,workspace,revision]);
  let request=indexes.get(key);
  if(!request){
    if(indexes.size>=16)indexes.delete(indexes.keys().next().value!);
    request=invoke<{files:SourceFile[]}>('project_source_index',{requestId,workspace,revision}).then(result=>{
      if(!Array.isArray(result.files))throw new Error('Project index is unavailable');return result;
    }).catch(error=>{indexes.delete(key);throw error;});
    indexes.set(key,request);
  }
  return request;
}
export function modelCaption(builder:Builder){
  return builder.id.split(':')[0].replace(/_/g,' ').replace(/^./,letter=>letter.toUpperCase());
}
export function indexedModels(files:SourceFile[]):IndexedModel[]{
  return files.flatMap(file=>file.builders
    .filter(builder=>builder.available&&(builder.kind==='geometry_candidate'||builder.kind==='part'||builder.kind==='assembly'))
    .filter(builder=>!file.builders.some(other=>other.id.startsWith(builder.id+':')&&other.available))
    .map(builder=>({file,builder})));
}
const title=(name:string)=>name.replace(/\.py$/,'').replace(/_/g,' ').replace(/^./,letter=>letter.toUpperCase());
const byName=(a:string,b:string)=>a.localeCompare(b,undefined,{sensitivity:'base'});
export function SourceCatalog({requestId,workspace,revision,disabled,onChoose,compact=false,onNavigate,filterQuery,compactView='models',onLoadingChange,activeModel,openingModel,visible=true}:{requestId?:string;workspace?:string;revision?:string;disabled?:boolean;onChoose:(path:string,builder:string)=>void;compact?:boolean;onNavigate?:()=>void;filterQuery?:string;compactView?:'models'|'files';onLoadingChange?:(loading:boolean)=>void;activeModel?:{entry:string;builder:string};openingModel?:{entry:string;builder:string};visible?:boolean}){
  const [sourcePath,setSourcePath]=useState<string|null>(null);
  const catalog=useRef<HTMLDivElement>(null);
  const [files,setFiles]=useState<SourceFile[]>([]),[loading,setLoading]=useState(true),[error,setError]=useState('');
  const loadingCallback=useRef(onLoadingChange);loadingCallback.current=onLoadingChange;
  useEffect(()=>{loadingCallback.current?.(loading);},[loading]);
  const [query,setQuery]=useState(''),[limit,setLimit]=useState(60),[attempt,setAttempt]=useState(0),[view,setView]=useState<'parts'|'files'>('parts');
  const key=requestId||workspace||'';
  const location=useSyncExternalStore(listener=>{listeners.add(listener);return()=>{listeners.delete(listener);};},()=>locations.get(key)||'');
  const go=(path:string)=>{locations.set(key,path);setQuery('');for(const listener of listeners)listener();onNavigate?.();};
  useEffect(()=>{setQuery('');setLimit(60);},[location]);
  useEffect(()=>{
    let active=true;setLoading(true);setError('');setSourcePath(null);
    loadSourceIndex(requestId,workspace,revision).then(result=>{if(active)setFiles(result.files);})
      .catch(error=>{if(active)setError(errorSummary(JSON.stringify(error)));}).finally(()=>{if(active)setLoading(false);});
    return()=>{active=false;};
  },[requestId,workspace,revision,attempt]);
  const highlighted=openingModel??activeModel;
  useEffect(()=>{
    if(!compact||!highlighted)return;
    const row=catalog.current?.querySelector<HTMLElement>('[aria-current="true"]');
    // Reopen ancestors even when a user collapsed an already active folder.
    for(let parent=row?.parentElement;parent&&parent!==catalog.current;parent=parent.parentElement){
      if(parent.tagName==='DETAILS')(parent as HTMLDetailsElement).open=true;
    }
    // Only scroll the tree's own pane, after the section is actually expanded.
    const pane=catalog.current?.closest<HTMLElement>('.explorer-section-host');
    if(!visible||!row||!pane||!pane.clientHeight)return;
    const bounds=pane.getBoundingClientRect(),entry=row.getBoundingClientRect();
    const top=bounds.top+pane.clientTop,bottom=top+pane.clientHeight;
    if(entry.top<top)pane.scrollTop+=entry.top-top;
    else if(entry.bottom>bottom)pane.scrollTop+=entry.bottom-bottom;
  },[compact,compactView,highlighted?.entry,highlighted?.builder,files,filterQuery,visible]);
  const search=(filterQuery??query).trim().toLowerCase();
  const matching=files.map(file=>({...file,builders:file.builders.filter(builder=>`${file.path} ${builder.label} ${file.description}`.toLowerCase().includes(search))}))
    .filter(file=>file.builders.length||file.path.toLowerCase().includes(search));
  const groups=matching.map(file=>({...file,builders:indexedModels([file]).map(item=>item.builder)})).filter(file=>file.builders.length);
  const selected=groups.find(file=>'file:'+file.path===location||location.startsWith('entry:'+file.path+'#'));
  const activeFolder=compact&&highlighted?highlighted.entry.split('/').slice(0,-1).join('/'):'';
  const folder=selected?selected.path.split('/').slice(0,-1).join('/'):location.startsWith('folder:')?location.slice(7):'';
  const parts=(search?groups:selected?[selected]:[]).flatMap(file=>file.builders.map(builder=>({file,builder})));
  type Tree={folders:Map<string,Tree>;files:SourceFile[];count:number};
  const tree:Tree={folders:new Map(),files:[],count:0};
  for(const file of (compact?compactView==='files'?matching:groups:view==='files'?matching:groups)){
    const segments=file.path.split('/');segments.pop();let node=tree;node.count+=file.builders.length;
    for(const segment of segments){if(!node.folders.has(segment))node.folders.set(segment,{folders:new Map(),files:[],count:0});node=node.folders.get(segment)!;node.count+=file.builders.length;}
    node.files.push(file);
  }
  const caption=modelCaption;
  const openEntry=(file:SourceFile,builder:Builder)=>{
    locations.set(key,'entry:'+file.path+'#'+builder.id);for(const listener of listeners)listener();
    onChoose(file.path,builder.id);
  };
  let current=tree;for(const segment of folder.split('/').filter(Boolean))current=current.folders.get(segment)||current;
  const sortedFolders=(node:Tree)=>[...node.folders].sort(([a],[b])=>byName(a,b));
  const sortedFiles=(node:Tree)=>[...node.files].sort((a,b)=>byName(a.path,b.path));
  const sortedModels=(node:Tree)=>sortedFiles(node).flatMap(file=>file.builders.map(builder=>({file,builder}))).sort((a,b)=>byName(caption(a.builder),caption(b.builder))||byName(a.file.path,b.file.path)||byName(a.builder.id,b.builder.id));
  const fileTree=(node:Tree,prefix=''):React.ReactNode=><ul>{sortedFolders(node).map(([name,child])=><li key={name}><details open={!!search||activeFolder===prefix+name||activeFolder.startsWith(prefix+name+'/')||folder===prefix+name||folder.startsWith(prefix+name+'/')}><summary><Icon name="folder" size={14}/>{name}</summary>{fileTree(child,prefix+name+'/')}</details></li>)}{compact&&compactView==='models'?sortedModels(node).map(({file,builder})=><li key={`${file.path}:${builder.id}`}><button type="button" className="library-part-link" aria-busy={openingModel?.entry===file.path&&openingModel.builder===builder.id||undefined} title={`${file.path}:${builder.line} · ${builder.label}`} aria-current={highlighted?.entry===file.path&&highlighted.builder===builder.id?'true':undefined} disabled={disabled} onClick={()=>openEntry(file,builder)}><Icon name="cube" size={13}/><span>{caption(builder)}</span>{openingModel?.entry===file.path&&openingModel.builder===builder.id&&<small className="library-opening-status" role="status" aria-label={`Opening ${caption(builder)}`}>Opening…</small>}{node.files.some(other=>other.path!==file.path&&other.builders.some(candidate=>caption(candidate)===caption(builder)))&&<small>{file.path.split('/').at(-1)}</small>}</button></li>):sortedFiles(node).map(file=><li key={file.path}>{compact?<button type="button" className="library-file-link" aria-current={activeModel?.entry===file.path?'true':undefined} title={file.path} onClick={()=>setSourcePath(file.path)}><Icon name="file" size={14}/>{file.path.split('/').at(-1)}</button>:<details open={!!search}><summary title={file.description}>{file.path.split('/').at(-1)}</summary>{file.diagnostic&&<p role="status">{file.diagnostic}</p>}<ul>{[...file.builders].sort((a,b)=>byName(a.label,b.label)||byName(a.id,b.id)).map(builder=><li key={builder.id}><button type="button" disabled={disabled||!builder.available} title={builder.reason||`Line ${builder.line}`} onClick={()=>onChoose(file.path,builder.id)}>{builder.label}</button></li>)}</ul></details>}</li>)}</ul>;
  if(compact)return <div ref={catalog} className="source-catalog catalog-compact" aria-busy={loading}>{error?<p role="alert">{error}</p>:<nav aria-label={compactView==='models'?'Indexed models':'Project source files'}>{fileTree(tree)}{!!search&&!(compactView==='models'?groups.length:matching.length)&&<p className="muted" role="status">{compactView==='models'?'No matching models.':'No matching files.'}</p>}</nav>}{sourcePath&&workspace&&revision&&<SourcePreview workspace={workspace} revision={revision} path={sourcePath} onClose={()=>setSourcePath(null)}/>}</div>;
  return <div className="source-catalog">
    <div className="library-toolbar"><div className="catalog-view" role="group" aria-label="Project library view"><button type="button" aria-pressed={view==='parts'} onClick={()=>{setView('parts');setLimit(60);}}>Geometry candidates</button><button type="button" aria-pressed={view==='files'} onClick={()=>setView('files')}>Files</button></div>
    <input type="search" className="explorer-search" aria-label="Search project" placeholder="Search entries…" value={query} onChange={event=>{setQuery(event.target.value);setLimit(60);}}/></div>
    {loading&&<span className="sidebar-section-loading" role="status" aria-label="Loading project entries"/>}
    {error&&<p role="alert">{error} <button type="button" onClick={()=>setAttempt(value=>value+1)}>Retry indexing</button></p>}
    {!loading&&!error&&(view==='files'?<div className="catalog-file-tree" aria-label="Project files">{fileTree(tree)}</div>:<>
      <nav className="library-breadcrumbs" aria-label="Library location"><button type="button" onClick={()=>go('')}>Geometry candidates</button>{folder.split('/').filter(Boolean).map((segment,index,array)=><React.Fragment key={index}><span>/</span><button type="button" onClick={()=>go('folder:'+array.slice(0,index+1).join('/'))}>{segment}</button></React.Fragment>)}{selected&&<><span>/</span><span>{title(selected.path.split('/').at(-1)!)}</span></>}</nav>
      <p className="library-inference">Entries are inferred from source. Building an entry determines the geometry it produces.</p>
      <header className="library-heading"><h2>{search?'Search results':selected?title(selected.path.split('/').at(-1)!):title(folder.split('/').at(-1)||'Geometry candidates')}</h2><p>{search?`${parts.length} matches`:selected?selected.description:`${current.folders.size} folders · ${current.count} geometry candidates`}</p></header>
      {search||selected?<ul className="library-part-list" aria-label="Source entries">{[...parts].sort((a,b)=>byName(caption(a.builder),caption(b.builder))||byName(a.file.path,b.file.path)||byName(a.builder.id,b.builder.id)).slice(0,limit).map(({file,builder})=><li key={`${file.path}:${builder.id}`}><Icon name="cube" size={17}/><div><strong>{caption(builder)}</strong><small>{search?file.path.split('/').at(-1)!:'Geometry candidate'}</small></div><button type="button" disabled={disabled} aria-label={`Open ${caption(builder)}`} onClick={()=>onChoose(file.path,builder.id)}>Open</button></li>)}</ul>:<ul className="library-group-list" aria-label="Source groups">{sortedFolders(current).map(([name,child])=><li key={name}><button type="button" onClick={()=>go('folder:'+(folder?folder+'/':'')+name)}><Icon name="folder" size={18}/><strong>{title(name)}</strong><small>{child.count} entries</small><span>›</span></button></li>)}{sortedModels(current).map(({file,builder})=><li key={`${file.path}:${builder.id}`}><button type="button" disabled={disabled} title={`${file.path}:${builder.line}`} onClick={()=>openEntry(file,builder)}><Icon name="cube" size={18}/><strong>{caption(builder)}</strong><small>Geometry candidate</small><span>Open</span></button></li>)}</ul>}
      {parts.length>limit&&<button type="button" onClick={()=>setLimit(value=>value+60)}>Show more entries</button>}
      {search&&!parts.length&&<p role="status">No matching entries.</p>}
      {!groups.length&&!search&&<p role="status">No geometry candidates found. Browse Files to inspect the source.</p>}
    </>)}

  </div>;
}
