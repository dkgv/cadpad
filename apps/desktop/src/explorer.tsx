import React,{useEffect,useId,useMemo,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
import type {SceneModel} from '@cadpad/viewer';
import {Icon} from './ui';
import {SourceCatalog} from './source-catalog';
import {SidebarSections} from './sidebar-sections';
import {MeshInspect} from './mesh-inspect';
import {createPortal} from 'react-dom';
import type {DockTab} from './dock-layout';

export function ProjectExplorer({workspace,revision,projectName,catalogWorkspace=workspace,catalogRevision=revision,activeModel,openingModel,selectionPanel,builderDisabled,onChooseBuilder,onBrowseLibrary,parts,assets,parameters,selected,hidden,onSelect,onToggleVisibility,onShowAll}:{
  workspace:string;revision:string;projectName?:string;catalogWorkspace?:string;catalogRevision?:string;activeModel?:{entry:string;builder:string};openingModel?:{entry:string;builder:string};selectionPanel?:React.ReactNode;builderDisabled?:boolean;onBrowseLibrary?:()=>void;onChooseBuilder:(path:string,builder:string)=>void;parameters?:React.ReactNode;parts:SceneModel['parts'];assets:Record<string,string>;selected?:string;
  hidden:Set<string>;onSelect:(id:string)=>void;onToggleVisibility:(id:string)=>void;onShowAll:()=>void;
}){
  const [query,setQuery]=useState('');
  const [meshes,setMeshes]=useState<string[]>([]);
  const [inspect,setInspect]=useState<string|null>(null);
  const prefix=useId();
  const ids=useMemo(()=>Object.fromEntries(['components','files','objects','parameters','references','selection'].map(name=>[name,`${prefix}-${name}`])),[prefix]);
  const hosts=useMemo(()=>Object.fromEntries(Object.values(ids).map(id=>{const host=document.createElement('div');host.className='explorer-section-host';return [id,host];})),[ids]);
  const [expanded,setExpanded]=useState(()=>[ids.components,ids.parameters,ids.selection]);
  const [indexing,setIndexing]=useState<Record<string,boolean>>({});
  const [weights,setWeights]=useState<Record<string,number>>({});
  useEffect(()=>{setQuery('');setInspect(null);},[workspace]);
  useEffect(()=>{
    let active=true;
    invoke<{files?:unknown}>('project_meshes',{workspace}).then(result=>{
      if(!active)return;
      const files=Array.isArray(result?.files)?result.files.filter((path):path is string=>typeof path==='string'&&path.toLowerCase().endsWith('.stl')):[];
      setMeshes(files);
    }).catch(()=>{if(active)setMeshes([]);});
    return()=>{active=false;};
  },[workspace,revision]);
  useEffect(()=>{
    if(!activeModel)return;
    setQuery('');
    setExpanded(values=>values.includes(ids.components)?values:[...values,ids.components]);
  },[catalogWorkspace,activeModel?.entry,activeModel?.builder,ids.components]);
  const search=query.trim().toLocaleLowerCase();
  const matches=(name:string)=>name.toLocaleLowerCase().includes(search);
  const visibleParts=parts.filter(part=>matches(part.name));
  const assetNames=Object.keys(assets).sort((a,b)=>a.localeCompare(b));
  const meshPaths=[...new Set([...meshes,...assetNames.filter(path=>path.toLowerCase().endsWith('.stl')).map(path=>path.toLowerCase().startsWith('assets/')?path:`assets/${path}`)])].sort((a,b)=>a.localeCompare(b));
  const covered=new Set(meshPaths);
  const staticAssets=assetNames.filter(path=>!covered.has(path)&&!covered.has(`assets/${path}`)&&!path.toLowerCase().endsWith('.stl'));
  const matchingMeshes=meshPaths.filter(matches);
  const matchingAssets=staticAssets.filter(matches);
  const contents:Record<string,React.ReactNode>={};
  const tabs:Record<string,DockTab>={};
  const section=(name:string,title:string,content:React.ReactNode)=>{tabs[ids[name]]={id:ids[name],title};contents[ids[name]]=content;};
  section('components','Models',<SourceCatalog compact visible={!!search||expanded.includes(ids.components)} activeModel={activeModel} openingModel={openingModel} onLoadingChange={loading=>setIndexing(values=>values[ids.components]===loading?values:{...values,[ids.components]:loading})} filterQuery={query} onNavigate={onBrowseLibrary} disabled={builderDisabled} workspace={catalogWorkspace} revision={catalogRevision} onChoose={onChooseBuilder}/>);
  section('files','Files',<SourceCatalog compact visible={!!search||expanded.includes(ids.files)} activeModel={activeModel} onLoadingChange={loading=>setIndexing(values=>values[ids.files]===loading?values:{...values,[ids.files]:loading})} compactView="files" filterQuery={query} workspace={catalogWorkspace} revision={catalogRevision} onChoose={onChooseBuilder}/>);
  section('objects','Objects',parts.length?<div className="explorer-loaded-model">
    {!!search&&!visibleParts.length&&<p className="muted" role="status">No matching objects.</p>}
    {!!hidden.size&&<button className="explorer-show-all" onClick={onShowAll}>Show all objects</button>}
    <ul className="explorer-parts" aria-label="Objects in this model">{visibleParts.map(part=><li className={`part-row ${selected===part.instance_id?'selected':''} ${hidden.has(part.instance_id)?'is-hidden':''}`} key={part.instance_id}>
      <button className="part-select" aria-pressed={selected===part.instance_id} onClick={()=>onSelect(part.instance_id)}><Icon name="cube" size={17}/><span className="explorer-part-caption"><span>{part.name}</span><small>{part.accuracy==='exact'?'CAD geometry':'Mesh reference'}</small></span></button>
      <button className="visibility-toggle" aria-label={`${hidden.has(part.instance_id)?'Show':'Hide'} ${part.name}`} title={hidden.has(part.instance_id)?'Show object':'Hide object'} onClick={()=>onToggleVisibility(part.instance_id)}><Icon name={hidden.has(part.instance_id)?'eyeOff':'eye'} size={17}/></button>
    </li>)}</ul>
  </div>:<p className="explorer-section-empty">No objects in the active model.</p>);
  if(React.Children.count(parameters))section('parameters','Parameters',parameters);
  if(meshPaths.length||assetNames.length)section('references','Reference files',<>
    {!!search&&!matchingMeshes.length&&!matchingAssets.length&&<p className="muted" role="status">No matching files.</p>}
    <ul className="explorer-assets" aria-label="Reference files">{matchingMeshes.map(path=><li key={path}><button type="button" className="explorer-mesh" title={path} onClick={()=>setInspect(path)}><Icon name="file" size={15}/><span>{path.split('/').at(-1)}</span><small>STL</small></button></li>)}{matchingAssets.map(path=><li key={path} title={path}><Icon name="file" size={15}/><span>{path.split('/').at(-1)}</span><small>{path.includes('.')?path.split('.').at(-1)?.toUpperCase():'File'}</small></li>)}</ul>
  </>);
  if(selectionPanel)section('selection','Selection',selectionPanel);
  const order=Object.keys(tabs);
  return <div className="project-explorer">
    <div className="explorer-filter"><Icon name="search" size={15}/><input type="search" aria-label="Search Explorer" placeholder="Search Explorer…" value={query} onChange={event=>setQuery(event.target.value)} onKeyDown={event=>{if(event.key==='Escape')setQuery('');}}/>{query&&<button type="button" className="icon-button" aria-label="Clear Explorer search" onClick={()=>setQuery('')}><Icon name="close" size={13}/></button>}</div>
    <SidebarSections sectionMode loading={indexing} area="left" group={{kind:'group',id:prefix,tabs:order,active:order[0]}} tabs={tabs} hosts={hosts} expanded={search?[...new Set([...expanded,ids.components,ids.files,ids.objects,ids.references])]:expanded} weights={weights} onToggle={id=>setExpanded(values=>values.includes(id)?values.filter(value=>value!==id):[...values,id])} onDrop={()=>{}} onResize={next=>setWeights(values=>({...values,...next}))}/>
    {Object.entries(contents).map(([id,content])=>createPortal(content,hosts[id],id))}
    {inspect&&createPortal(<MeshInspect workspace={workspace} path={inspect} onClose={()=>setInspect(null)}/>,document.body)}

  </div>;
}
