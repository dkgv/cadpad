import React,{useEffect,useMemo,useRef,useState,useSyncExternalStore} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {Icon,errorSummary} from './ui';
import {SourcePreview} from './source-preview';

export type Builder={id:string;label:string;available:boolean;reason:string;line:number;kind?:'geometry_candidate'|'part'|'assembly'|'function'};
export type SourceFile={path:string;description:string;builders:Builder[];diagnostic:string|null;recommended?:string|null};
export type IndexedModel={file:SourceFile;builder:Builder};
const indexes=new Map<string,Promise<{files:SourceFile[]}>>();
const locations=new Map<string,string>();
const listeners=new Set<()=>void>();
let epoch=0;
function subscribe(listener:()=>void){listeners.add(listener);return()=>{listeners.delete(listener);};}
export function useSourceCatalogEpoch(){return useSyncExternalStore(subscribe,()=>epoch);}
export function resetSourceCatalogCache(){indexes.clear();locations.clear();epoch+=1;for(const listener of listeners)listener();}
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
const GIT_CHANGES=['modified','added','untracked','deleted','conflict'] as const;
export type GitChange=(typeof GIT_CHANGES)[number];
export type GitEntry={path:string;change:GitChange;directory:boolean};
const GIT_RANK:Record<GitChange,number>={untracked:1,added:2,modified:3,deleted:4,conflict:5};
const GIT_LABEL:Record<GitChange,string>={untracked:'New',added:'New',modified:'Changed',deleted:'Deleted',conflict:'Conflict'};
function isGitChange(value:string):value is GitChange{return (GIT_CHANGES as readonly string[]).includes(value);}
export function readGitStatus(value:unknown):{repo:boolean;entries:GitEntry[]}|null{
  if(!value||typeof value!=='object')return null;
  const record=value as {repo?:unknown;entries?:unknown};
  if(typeof record.repo!=='boolean'||!Array.isArray(record.entries))return null;
  const entries:GitEntry[]=[];
  for(const item of record.entries){
    if(!item||typeof item!=='object')continue;
    const entry=item as {path?:unknown;change?:unknown;directory?:unknown};
    if(typeof entry.path!=='string'||typeof entry.change!=='string'||!isGitChange(entry.change))continue;
    const directory=entry.directory===true||entry.path.endsWith('/');
    const path=entry.path.replaceAll('\\','/').replace(/^\.\//,'').replace(/\/+$/,'');
    if(!path||path.split('/').some(part=>!part||part==='.'||part==='..'))continue;
    entries.push({path,change:entry.change,directory});
  }
  return {repo:record.repo,entries};
}
function sameEntries(left:GitEntry[],right:GitEntry[]){
  return left.length===right.length&&left.every((entry,index)=>entry.path===right[index].path&&entry.change===right[index].change&&entry.directory===right[index].directory);
}
export function indexGit(entries:GitEntry[]){
  const files=new Map<string,GitChange>(),folders=new Map<string,GitChange>(),dirs:GitEntry[]=[];
  const bump=(map:Map<string,GitChange>,path:string,change:GitChange)=>{const current=map.get(path);if(!current||GIT_RANK[change]>GIT_RANK[current])map.set(path,change);};
  for(const entry of entries){
    if(entry.directory)dirs.push(entry);else bump(files,entry.path,entry.change);
    const parts=entry.path.split('/');if(!entry.directory)parts.pop();
    let prefix='';
    for(const part of parts){prefix=prefix?`${prefix}/${part}`:part;bump(folders,prefix,entry.change);}
  }
  return {files,folders,dirs};
}
export function fileGit(path:string,git:ReturnType<typeof indexGit>){
  const direct=git.files.get(path);if(direct)return direct;
  let best:GitChange|undefined;
  for(const dir of git.dirs)if(path===dir.path||path.startsWith(`${dir.path}/`)){if(!best||GIT_RANK[dir.change]>GIT_RANK[best])best=dir.change;}
  return best;
}
export function folderGit(path:string,git:ReturnType<typeof indexGit>){return git.folders.get(path);}
function gitMark(change?:GitChange){if(!change)return '';return change==='added'||change==='untracked'?'git-new':`git-${change}`;}
export function SourceCatalog({requestId,workspace,revision,disabled,onChoose,compact=false,onNavigate,filterQuery,compactView='models',onLoadingChange,activeModel,openingModel,visible=true}:{requestId?:string;workspace?:string;revision?:string;disabled?:boolean;onChoose:(path:string,builder:string)=>void;compact?:boolean;onNavigate?:()=>void;filterQuery?:string;compactView?:'models'|'files';onLoadingChange?:(loading:boolean)=>void;activeModel?:{entry:string;builder:string};openingModel?:{entry:string;builder:string};visible?:boolean}){
  const [sourcePath,setSourcePath]=useState<string|null>(null);
  const catalog=useRef<HTMLDivElement>(null);
  const [files,setFiles]=useState<SourceFile[]>([]),[loading,setLoading]=useState(true),[error,setError]=useState('');
  const loadingCallback=useRef(onLoadingChange);loadingCallback.current=onLoadingChange;
  useEffect(()=>{loadingCallback.current?.(loading);},[loading]);
  const [query,setQuery]=useState(''),[limit,setLimit]=useState(60),[attempt,setAttempt]=useState(0),[view,setView]=useState<'parts'|'files'>('parts');
  const [gitEntries,setGitEntries]=useState<GitEntry[]>([]);
  const gitProject=useRef(''),git=useMemo(()=>indexGit(gitEntries),[gitEntries]);
  const showGit=compact?compactView==='files':view==='files';
  const key=requestId||workspace||'';
  const catalogEpoch=useSourceCatalogEpoch();
  const location=useSyncExternalStore(subscribe,()=>locations.get(key)||'');
  const go=(path:string)=>{locations.set(key,path);setQuery('');for(const listener of listeners)listener();onNavigate?.();};
  useEffect(()=>{setQuery('');setLimit(60);},[location]);
  useEffect(()=>{
    let active=true;setLoading(true);setError('');setSourcePath(null);
    loadSourceIndex(requestId,workspace,revision).then(result=>{if(active)setFiles(result.files);})
      .catch(error=>{if(active)setError(errorSummary(JSON.stringify(error)));}).finally(()=>{if(active)setLoading(false);});
    return()=>{active=false;};
  },[requestId,workspace,revision,attempt,catalogEpoch]);
  useEffect(()=>{
    if(!workspace||!showGit){gitProject.current='';setGitEntries(current=>current.length?[]:current);return;}
    if(gitProject.current!==workspace){gitProject.current=workspace;setGitEntries(current=>current.length?[]:current);}
    if(compact&&!visible)return;
    let active=true,pending=false;
    const load=()=>{
      if(pending)return;pending=true;
      invoke<unknown>('project_git_status',{workspace}).then(value=>{
        if(!active)return;
        const status=readGitStatus(value);if(!status)return;
        const next=status.repo?status.entries:[];
        setGitEntries(current=>sameEntries(current,next)?current:next);
      }).catch(()=>undefined).finally(()=>{pending=false;});
    };
    load();
    const timer=window.setInterval(load,4000);
    window.addEventListener('focus',load);
    return()=>{active=false;window.clearInterval(timer);window.removeEventListener('focus',load);};
  },[workspace,revision,showGit,compact,visible]);
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
  const fileTree=(node:Tree,prefix=''):React.ReactNode=><ul>{sortedFolders(node).map(([name,child])=>{
    const path=prefix+name,change=showGit?folderGit(path,git):undefined,mark=gitMark(change);
    return <li key={name}><details open={!!search||activeFolder===path||activeFolder.startsWith(path+'/')||folder===path||folder.startsWith(path+'/')}><summary className={mark||undefined} title={change?GIT_LABEL[change]:undefined} aria-label={change?`${name}, ${GIT_LABEL[change]}`:undefined}><Icon name="folder" size={14}/><span className="tree-label">{name}</span>{mark?<i className="git-dot" aria-hidden="true"/>:null}</summary>{fileTree(child,path+'/')}</details></li>;
  })}{compact&&compactView==='models'?sortedModels(node).map(({file,builder})=><li key={`${file.path}:${builder.id}`}><button type="button" className="library-part-link" aria-busy={openingModel?.entry===file.path&&openingModel.builder===builder.id||undefined} title={`${file.path}:${builder.line} · ${builder.label}`} aria-current={highlighted?.entry===file.path&&highlighted.builder===builder.id?'true':undefined} disabled={disabled} onClick={()=>openEntry(file,builder)}><Icon name="cube" size={13}/><span>{caption(builder)}</span>{openingModel?.entry===file.path&&openingModel.builder===builder.id&&<small className="library-opening-status" role="status" aria-label={`Opening ${caption(builder)}`}>Opening…</small>}{node.files.some(other=>other.path!==file.path&&other.builders.some(candidate=>caption(candidate)===caption(builder)))&&<small>{file.path.split('/').at(-1)}</small>}</button></li>):sortedFiles(node).map(file=>{
    const name=file.path.split('/').at(-1)!,change=showGit?fileGit(file.path,git):undefined,mark=gitMark(change);
    return <li key={file.path}>{compact?<button type="button" className={`library-file-link${mark?` ${mark}`:''}`} aria-current={activeModel?.entry===file.path?'true':undefined} title={file.path} aria-label={change?`${name}, ${GIT_LABEL[change]}`:undefined} onClick={()=>setSourcePath(file.path)}><Icon name="file" size={14}/><span className="tree-label">{name}</span>{mark?<i className="git-dot" aria-hidden="true"/>:null}</button>:<details open={!!search}><summary className={mark||undefined} title={change?`${file.description} · ${GIT_LABEL[change]}`:file.description} aria-label={change?`${name}, ${GIT_LABEL[change]}`:undefined}><span className="tree-label">{name}</span>{mark?<i className="git-dot" aria-hidden="true"/>:null}</summary>{file.diagnostic&&<p role="status">{file.diagnostic}</p>}<ul>{[...file.builders].sort((a,b)=>byName(a.label,b.label)||byName(a.id,b.id)).map(builder=><li key={builder.id}><button type="button" disabled={disabled||!builder.available} title={builder.reason||`Line ${builder.line}`} onClick={()=>onChoose(file.path,builder.id)}>{builder.label}</button></li>)}</ul></details>}</li>;
  })}</ul>;
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
