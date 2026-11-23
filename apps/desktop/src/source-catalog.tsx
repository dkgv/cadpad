import React,{useEffect,useMemo,useRef,useState,useSyncExternalStore} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {Icon,errorText} from './ui';
import {SourcePreview} from './source-preview';
import {watchProjectFiles} from './project-files';

export type Builder={id:string;label:string;available:boolean;reason:string;line:number;kind?:'geometry_candidate'|'part'|'assembly'|'function'|'model_file'};
export type SourceFile={path:string;description:string;builders:Builder[];diagnostic:string|null;recommended?:string|null;source?:string};
export type IndexedModel={file:SourceFile;builder:Builder};
type SourceIndex={files:SourceFile[];fingerprint?:string};
type BranchOverrides=Record<string,{context:string;open:boolean}>;
function readBranches(key:string):BranchOverrides{
  try{
    const saved:unknown=JSON.parse(window.localStorage.getItem(key)??'null');
    if(!saved||typeof saved!=='object'||Array.isArray(saved))return {};
    return Object.fromEntries(Object.entries(saved).filter(([,value])=>typeof value?.context==='string'&&typeof value.open==='boolean'));
  }catch{return {};}
}
const indexes=new Map<string,Promise<SourceIndex>>();
const refreshing=new Map<string,{promise:Promise<SourceIndex>;reading:boolean;dirty:boolean}>();
const locations=new Map<string,string>();
const listeners=new Set<()=>void>();
let epoch=0;
function subscribe(listener:()=>void){listeners.add(listener);return()=>{listeners.delete(listener);};}
export function useSourceCatalogEpoch(){return useSyncExternalStore(subscribe,()=>epoch);}
export function resetSourceCatalogCache(){indexes.clear();refreshing.clear();locations.clear();epoch+=1;for(const listener of listeners)listener();}
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
export function refreshSourceIndex(requestId?:string,workspace?:string,revision?:string){
  const key=JSON.stringify([requestId,workspace,revision]);
  const pending=refreshing.get(key);
  if(pending){if(pending.reading)pending.dirty=true;return pending.promise;}
  const state={promise:null as unknown as Promise<SourceIndex>,reading:false,dirty:false};
  state.promise=(async()=>{
    let result:SourceIndex;
    do{
      state.dirty=false;
      const cached=indexes.get(key);
      const previous=await cached?.catch(()=>undefined);
      state.reading=true;
      const next=await invoke<SourceIndex&{unchanged?:boolean}>('project_source_index',{requestId,workspace,revision,fingerprint:previous?.fingerprint});
      state.reading=false;
      if(next.unchanged&&previous)result=previous;
      else{
        if(!Array.isArray(next.files))throw new Error('Project index is unavailable');
        result=next;
        // A project switch or explicit reset may have invalidated this request.
        if(indexes.get(key)===cached){
          indexes.set(key,Promise.resolve(result));epoch+=1;for(const listener of listeners)listener();
        }
      }
    }while(state.dirty&&refreshing.get(key)===state);
    return result;
  })().finally(()=>{if(refreshing.get(key)===state)refreshing.delete(key);});
  refreshing.set(key,state);return state.promise;
}
export function modelCaption(builder:Builder){
  if(builder.kind==='model_file')return builder.label;
  return builder.id.split(':')[0].replace(/_/g,' ').replace(/^./,letter=>letter.toUpperCase());
}
export function indexedModels(files:SourceFile[]):IndexedModel[]{
  return files.flatMap(file=>file.builders
    .filter(builder=>builder.available&&(builder.kind==='geometry_candidate'||builder.kind==='part'||builder.kind==='assembly'||builder.kind==='model_file'))
    .filter(builder=>!file.builders.some(other=>other.id.startsWith(builder.id+':')&&other.available))
    .map(builder=>({file,builder})));
}
const title=(name:string)=>name.replace(/\.py$/,'').replace(/_/g,' ').replace(/^./,letter=>letter.toUpperCase());
const byName=new Intl.Collator(undefined,{sensitivity:'base'}).compare;
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
/** Drag payload for an Explorer folder dropped on the Assistant. */
export const FOLDER_DRAG_TYPE='application/x-cadpad-folder';
function gitMark(change?:GitChange){if(!change)return '';return change==='added'||change==='untracked'?'git-new':`git-${change}`;}
export function SourceCatalog({requestId,workspace,revision,disabled,onChoose,compact=false,filterQuery,compactView='models',onLoadingChange,activeModel,openingModel,visible=true,onReferenceFolder}:{onReferenceFolder?:(path:string)=>void;requestId?:string;workspace?:string;revision?:string;disabled?:boolean;onChoose:(path:string,builder:string)=>void;compact?:boolean;filterQuery?:string;compactView?:'models'|'files';onLoadingChange?:(loading:boolean)=>void;activeModel?:{entry:string;builder:string};openingModel?:{entry:string;builder:string};visible?:boolean}){
  const [sourcePath,setSourcePath]=useState<string|null>(null);
  const catalog=useRef<HTMLDivElement>(null);
  const [files,setFiles]=useState<SourceFile[]>([]),[loading,setLoading]=useState(true),[error,setError]=useState('');
  const hasFiles=useRef(false);
  const loadingCallback=useRef(onLoadingChange);loadingCallback.current=onLoadingChange;
  useEffect(()=>{loadingCallback.current?.(loading);},[loading]);
  const [query,setQuery]=useState(''),[limit,setLimit]=useState(60),[attempt,setAttempt]=useState(0),[view,setView]=useState<'parts'|'files'>('parts');
  const [gitEntries,setGitEntries]=useState<GitEntry[]>([]);
  const gitProject=useRef(''),git=useMemo(()=>indexGit(gitEntries),[gitEntries]);
  const lazyModels=compact&&compactView==='models';
  const showGit=compact?compactView==='files':view==='files';
  const branchStorage=compact&&workspace?`cadpad.explorer-folders.v1:${compactView}:${workspace}`:'';
  const [branches,setBranches]=useState(()=>branchStorage?readBranches(branchStorage):{});
  const [searchBranches,setSearchBranches]=useState<BranchOverrides>({});
  useEffect(()=>{
    setBranches(branchStorage?readBranches(branchStorage):{});
    setSearchBranches({});
  },[branchStorage,requestId]);
  const key=requestId||workspace||'';
  const catalogEpoch=useSourceCatalogEpoch();
  const location=useSyncExternalStore(subscribe,()=>locations.get(key)||'');
  const go=(path:string)=>{locations.set(key,path);setQuery('');for(const listener of listeners)listener();};
  useEffect(()=>{setQuery('');setLimit(60);},[location]);
  useEffect(()=>{hasFiles.current=false;},[requestId,workspace,revision]);
  useEffect(()=>{
    let active=true;
    if(!hasFiles.current){setLoading(true);setError('');setSourcePath(null);}
    loadSourceIndex(requestId,workspace,revision).then(result=>{if(!active)return;hasFiles.current=true;setFiles(result.files);setError('');})
      .catch(error=>{if(active)setError(errorText(error));}).finally(()=>{if(active)setLoading(false);});
    return()=>{active=false;};
  },[requestId,workspace,revision,attempt,catalogEpoch]);
  useEffect(()=>{
    if(!workspace)return;
    let active=true;
    const refresh=()=>{void refreshSourceIndex(requestId,workspace,revision).then(()=>{if(active)setError('');}).catch(error=>{if(active)setError(errorText(error));});};
    const close=watchProjectFiles(workspace,refresh,error=>{if(active)setError(errorText(error));});
    return()=>{active=false;close();};
  },[requestId,workspace,revision]);
  useEffect(()=>{
    if(!workspace||!showGit){gitProject.current='';setGitEntries(current=>current.length?[]:current);return;}
    if(gitProject.current!==workspace){gitProject.current=workspace;setGitEntries(current=>current.length?[]:current);}
    if(compact&&!visible)return;
    let active=true,pending=false,again=false;
    const load=()=>{
      if(pending){again=true;return;}pending=true;again=false;
      invoke<unknown>('project_git_status',{workspace}).then(value=>{
        if(!active)return;
        const status=readGitStatus(value);if(!status)return;
        const next=status.repo?status.entries:[];
        setGitEntries(current=>sameEntries(current,next)?current:next);
      }).catch(()=>undefined).finally(()=>{pending=false;if(active&&again)load();});
    };
    load();
    const close=watchProjectFiles(workspace,load);
    return()=>{active=false;close();};
  },[workspace,revision,showGit,compact,visible]);
  useEffect(()=>{setSourcePath(path=>path&&files.some(file=>file.path===path)?path:null);},[files]);
  const highlighted=openingModel??activeModel;
  useEffect(()=>{
    if(!compact||!highlighted)return;
    const row=catalog.current?.querySelector<HTMLElement>('[aria-current="true"]');
    for(let parent=row?.parentElement;parent&&parent!==catalog.current;parent=parent.parentElement){
      if(parent.hasAttribute('data-manually-collapsed'))return;
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
  const matching=useMemo(()=>search?files.map(file=>({...file,builders:file.builders.filter(builder=>`${file.path} ${builder.label} ${file.description}`.toLowerCase().includes(search))}))
    .filter(file=>file.builders.length||file.path.toLowerCase().includes(search)):files,[files,search]);
  const groups=useMemo(()=>compact&&showGit?[]:matching.map(file=>({...file,builders:indexedModels([file]).map(item=>item.builder)})).filter(file=>file.builders.length),[matching,compact,showGit]);
  const selected=groups.find(file=>'file:'+file.path===location||location.startsWith('entry:'+file.path+'#'));
  const activeFolder=compact&&highlighted?highlighted.entry.split('/').slice(0,-1).join('/'):'';
  const folder=selected?selected.path.split('/').slice(0,-1).join('/'):location.startsWith('folder:')?location.slice(7):'';
  const parts=(search?groups:selected?[selected]:[]).flatMap(file=>file.builders.map(builder=>({file,builder})));
  type Tree={folders:Map<string,Tree>;files:SourceFile[];count:number};
  const tree=useMemo(()=>{
    const root:Tree={folders:new Map(),files:[],count:0};
    for(const file of showGit?matching:groups){
      const segments=file.path.split('/');segments.pop();
      let node=root;
      node.count+=file.builders.length;
      for(const segment of segments){
        let child=node.folders.get(segment);
        if(!child){child={folders:new Map(),files:[],count:0};node.folders.set(segment,child);}
        node=child;node.count+=file.builders.length;
      }
      node.files.push(file);
    }
    return root;
  },[groups,matching,showGit]);
  const caption=modelCaption;
  const openEntry=(file:SourceFile,builder:Builder)=>{
    locations.set(key,'entry:'+file.path+'#'+builder.id);for(const listener of listeners)listener();
    onChoose(file.path,builder.id);
  };
  let current=tree;for(const segment of folder.split('/').filter(Boolean))current=current.folders.get(segment)||current;
  const sortedFolders=(node:Tree)=>[...node.folders].sort(([a],[b])=>byName(a,b));
  const sortedFiles=(node:Tree)=>[...node.files].sort((a,b)=>byName(a.path,b.path));
  const sortedModels=(node:Tree)=>sortedFiles(node).flatMap(file=>file.builders.map(builder=>({file,builder}))).sort((a,b)=>byName(caption(a.builder),caption(b.builder))||byName(a.file.path,b.file.path)||byName(a.builder.id,b.builder.id));
  const branchContext=search||JSON.stringify([highlighted?.entry,highlighted?.builder]);
  const branch=(id:string,automatic:boolean,summary:React.ReactNode,render:()=>React.ReactNode,attributes?:React.HTMLAttributes<HTMLElement>)=>{
    const override=(search?searchBranches:branches)[id];
    const manual=override?.context===branchContext;
    const open=manual?override.open:automatic||!!override?.open;
    return <details open={open} data-manually-collapsed={manual&&!open?'':undefined}>
      <summary {...attributes} onClick={event=>{
        event.preventDefault();
        const change={context:branchContext,open:!open};
        if(search){setSearchBranches(current=>({...current,[id]:change}));return;}
        const next={...branches,[id]:change};
        setBranches(next);
        if(branchStorage)try{window.localStorage.setItem(branchStorage,JSON.stringify(next));}catch{}
      }}>{summary}</summary>
      {(open||!lazyModels)&&render()}
    </details>;
  };
  const modelTree=(node:Tree,prefix:string)=>{
    const entries=sortedModels(node);
    const groups=new Map<string,IndexedModel[]>(),namePaths=new Map<string,Set<string>>();
    for(const model of entries){
      const name=caption(model.builder);
      const paths=namePaths.get(name)??new Set<string>();
      paths.add(model.file.path);namePaths.set(name,paths);
      if(model.builder.kind!=='model_file')continue;
      const stem=model.builder.label.replace(/\.[^.]+$/,'');
      const group=groups.get(stem)??[];
      group.push(model);groups.set(stem,group);
    }
    const row=({file,builder}:IndexedModel)=><li key={`${file.path}:${builder.id}`}><button type="button" className="library-part-link" aria-busy={openingModel?.entry===file.path&&openingModel.builder===builder.id||undefined} title={builder.kind==='model_file'?file.path:`${file.path}:${builder.line} · ${builder.label}`} aria-current={highlighted?.entry===file.path&&highlighted.builder===builder.id?'true':undefined} disabled={disabled} onClick={()=>openEntry(file,builder)}><Icon name="cube" size={13}/><span>{caption(builder)}</span>{openingModel?.entry===file.path&&openingModel.builder===builder.id&&<small className="library-opening-status" role="status" aria-label={`Opening ${caption(builder)}`}>Opening…</small>}{(namePaths.get(caption(builder))?.size??0)>1&&<small>{file.path.split('/').at(-1)}</small>}</button></li>;
    return entries.map(model=>{
      if(model.builder.kind!=='model_file')return row(model);
      const stem=model.builder.label.replace(/\.[^.]+$/,'');
      const group=groups.get(stem)!;
      if(group.length===1)return row(model);
      if(group[0]!==model)return null;
      const selected=group.some(({file,builder})=>highlighted?.entry===file.path&&highlighted.builder===builder.id);
      return <li key={`formats:${stem}`}>{branch(`formats:${prefix}${stem}`,!!search||selected,
        <><Icon name="cube" size={13}/><span className="tree-label">{stem}</span></>,()=> <ul>{group.map(row)}</ul>)}</li>;
    });
  };
  const fileTree=(node:Tree,prefix=''):React.ReactNode=><ul>{sortedFolders(node).map(([name,child])=>{
    const path=prefix+name,change=showGit?folderGit(path,git):undefined,mark=gitMark(change);
    const automatic=!!search||activeFolder===path||activeFolder.startsWith(path+'/')||folder===path||folder.startsWith(path+'/');
    const summary=<><Icon name="folder" size={14}/><span className="tree-label">{name}</span>{mark?<i className="git-dot" aria-hidden="true"/>:null}{onReferenceFolder&&<button type="button" className="folder-chat-reference" title="Reference this folder in the Assistant" aria-label={`Reference ${path} in the Assistant`} onClick={event=>{event.preventDefault();event.stopPropagation();onReferenceFolder(path);}}><Icon name="chat" size={12}/></button>}</>;
    return <li key={name}>{compact?branch(`folder:${path}`,automatic,summary,()=>fileTree(child,path+'/'),{
      className:mark||undefined,title:change?GIT_LABEL[change]:undefined,'aria-label':change?`${name}, ${GIT_LABEL[change]}`:undefined,
      ...(onReferenceFolder?{draggable:true,onDragStart:event=>{event.dataTransfer.setData(FOLDER_DRAG_TYPE,path);event.dataTransfer.setData('text/plain',path);event.dataTransfer.effectAllowed='copy';}}:{}),
    }):
      <details open={automatic}><summary className={mark||undefined} title={change?GIT_LABEL[change]:undefined}
        aria-label={change?`${name}, ${GIT_LABEL[change]}`:undefined}>{summary}</summary>{fileTree(child,path+'/')}</details>}</li>;
  })}{lazyModels?modelTree(node,prefix):sortedFiles(node).map(file=>{
    const name=file.path.split('/').at(-1)!,change=showGit?fileGit(file.path,git):undefined,mark=gitMark(change);
    return <li key={file.path}>{compact?<button type="button" className={`library-file-link${mark?` ${mark}`:''}`} aria-current={activeModel?.entry===file.path?'true':undefined} title={file.path} aria-label={change?`${name}, ${GIT_LABEL[change]}`:undefined} disabled={file.builders[0]?.kind==='model_file'&&disabled} onClick={()=>{const builder=file.builders[0];if(builder?.kind==='model_file')openEntry(file,builder);else setSourcePath(file.path);}}><Icon name="file" size={14}/><span className="tree-label">{name}</span>{mark?<i className="git-dot" aria-hidden="true"/>:null}</button>:<details open={!!search}><summary className={mark||undefined} title={change?`${file.description} · ${GIT_LABEL[change]}`:file.description} aria-label={change?`${name}, ${GIT_LABEL[change]}`:undefined}><span className="tree-label">{name}</span>{mark?<i className="git-dot" aria-hidden="true"/>:null}</summary>{file.diagnostic&&<p role="status">{file.diagnostic}</p>}<ul>{[...file.builders].sort((a,b)=>byName(a.label,b.label)||byName(a.id,b.id)).map(builder=><li key={builder.id}><button type="button" disabled={disabled||!builder.available} title={builder.reason||`Line ${builder.line}`} onClick={()=>onChoose(file.path,builder.id)}>{builder.label}</button></li>)}</ul></details>}</li>;
  })}</ul>;
  if(compact)return <div ref={catalog} className="source-catalog catalog-compact" aria-busy={loading}>{error?<p role="alert">{error}</p>:<nav aria-label={compactView==='models'?'Indexed models':'Project source files'}>{fileTree(tree)}{!!search&&!(compactView==='models'?groups.length:matching.length)&&<p className="muted" role="status">{compactView==='models'?'No matching models.':'No matching files.'}</p>}</nav>}{sourcePath&&workspace&&revision&&<SourcePreview workspace={workspace} revision={revision} path={sourcePath} source={files.find(file=>file.path===sourcePath)?.source} onClose={()=>setSourcePath(null)}/>}</div>;
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
