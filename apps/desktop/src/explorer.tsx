import React,{useEffect,useId,useMemo,useRef,useState} from 'react';
import type {SceneModel} from '@cadpad/viewer';
import {Icon} from './ui';
import {SourceCatalog} from './source-catalog';
import {SidebarSections} from './sidebar-sections';
import {createPortal} from 'react-dom';
import type {DockTab} from './dock-layout';

type ExplorerProps={
  workspace:string;revision:string;catalogWorkspace?:string;catalogRevision?:string;activeModel?:{entry:string;builder:string};openingModel?:{entry:string;builder:string};selectionPanel?:React.ReactNode;builderDisabled?:boolean;onChooseBuilder:(path:string,builder:string)=>void;parameters?:React.ReactNode;parts:SceneModel['parts'];selected?:string;
  /** Attach an Explorer folder (project-relative path) to the next Assistant message. */
  onReferenceFolder?:(path:string)=>void;
  hidden:Set<string>;onSelect:(id:string)=>void;onToggleVisibility:(id:string)=>void;onShowAll:()=>void;
};
const sectionNames=['components','objects','parameters','selection'];
const defaultSections=['components','parameters','selection'];
const explorerKey=(workspace:string)=>`cadpad.explorer.v1:${workspace}`;

function readSections(workspace:string):{expanded:string[];weights:Record<string,number>}{
  try{
    const saved=JSON.parse(window.localStorage.getItem(explorerKey(workspace))??'null');
    return {
      expanded:Array.isArray(saved?.expanded)?sectionNames.filter(name=>saved.expanded.includes(name)):defaultSections,
      weights:Object.fromEntries(sectionNames.flatMap(name=>{
        const weight=saved?.weights?.[name];
        return Number.isFinite(weight)&&weight>0?[[name,weight]]:[];
      })),
    };
  }catch{return {expanded:defaultSections,weights:{}};}
}

export function ProjectExplorer(props:ExplorerProps){
  return <Explorer key={props.catalogWorkspace??props.workspace} {...props}/>;
}

function Explorer({workspace,revision,catalogWorkspace=workspace,catalogRevision=revision,activeModel,openingModel,selectionPanel,builderDisabled,onChooseBuilder,parts,parameters,selected,hidden,onSelect,onToggleVisibility,onShowAll,onReferenceFolder}:ExplorerProps){
  const [query,setQuery]=useState('');
  const prefix=useId();
  const ids=useMemo(()=>Object.fromEntries(sectionNames.map(name=>[name,`${prefix}-${name}`])),[prefix]);
  const hosts=useMemo(()=>Object.fromEntries(Object.values(ids).map(id=>{const host=document.createElement('div');host.className='explorer-section-host';return [id,host];})),[ids]);
  const [sections,setSections]=useState(()=>readSections(catalogWorkspace));
  const expanded=sections.expanded.map(name=>ids[name]);
  const weights=Object.fromEntries(Object.entries(sections.weights).map(([name,weight])=>[ids[name],weight]));
  const [indexing,setIndexing]=useState<Record<string,boolean>>({});
  useEffect(()=>{
    try{window.localStorage.setItem(explorerKey(catalogWorkspace),JSON.stringify(sections));}catch{}
  },[catalogWorkspace,sections]);
  useEffect(()=>{setQuery('');},[workspace]);
  const activeKey=JSON.stringify(activeModel?[activeModel.entry,activeModel.builder]:[]);
  const previousActive=useRef(activeKey);
  useEffect(()=>{
    if(previousActive.current===activeKey)return;
    previousActive.current=activeKey;
    if(activeKey==='[]')return;
    setQuery('');
    setSections(current=>current.expanded.includes('components')?current:{...current,expanded:[...current.expanded,'components']});
  },[activeKey]);
  const search=query.trim().toLocaleLowerCase();
  const matches=(name:string)=>name.toLocaleLowerCase().includes(search);
  const visibleParts=parts.filter(part=>matches(part.name));
  const contents:Record<string,React.ReactNode>={};
  const tabs:Record<string,DockTab>={};
  const section=(name:string,title:string,content:React.ReactNode)=>{tabs[ids[name]]={id:ids[name],title};contents[ids[name]]=content;};
  section('components','Models',<SourceCatalog compact visible={!!search||expanded.includes(ids.components)} activeModel={activeModel} openingModel={openingModel} onLoadingChange={loading=>setIndexing(values=>values[ids.components]===loading?values:{...values,[ids.components]:loading})} filterQuery={query} disabled={builderDisabled} workspace={catalogWorkspace} revision={catalogRevision} onChoose={onChooseBuilder} onReferenceFolder={onReferenceFolder}/>);
  section('objects','Parts',parts.length?<div className="explorer-loaded-model">
    {!!search&&!visibleParts.length&&<p className="muted" role="status">No matching parts.</p>}
    {!!hidden.size&&<button className="explorer-show-all" onClick={onShowAll}>Show all parts</button>}
    <ul className="explorer-parts" aria-label="Parts in this model">{visibleParts.map(part=><li className={`part-row ${selected===part.instance_id?'selected':''} ${hidden.has(part.instance_id)?'is-hidden':''}`} key={part.instance_id}>
      <button className="part-select" aria-pressed={selected===part.instance_id} onClick={()=>onSelect(part.instance_id)}><Icon name="cube" size={17}/><span className="explorer-part-caption"><span>{part.name}</span><small>{part.accuracy==='exact'?'CAD geometry':'Mesh reference'}</small></span></button>
      <button className="visibility-toggle" aria-label={`${hidden.has(part.instance_id)?'Show':'Hide'} ${part.name}`} title={hidden.has(part.instance_id)?'Show part':'Hide part'} onClick={()=>onToggleVisibility(part.instance_id)}><Icon name={hidden.has(part.instance_id)?'eyeOff':'eye'} size={17}/></button>
    </li>)}</ul>
  </div>:<p className="explorer-section-empty">No parts in the active model.</p>);
  if(React.Children.count(parameters))section('parameters','Parameters',parameters);
  if(selectionPanel)section('selection','Selection',selectionPanel);
  const order=Object.keys(tabs);
  return <div className="project-explorer">
    <div className="explorer-filter"><Icon name="search" size={15}/><input type="search" aria-label="Search Explorer" placeholder="Search Explorer…" value={query} onChange={event=>setQuery(event.target.value)} onKeyDown={event=>{if(event.key==='Escape')setQuery('');}}/>{query&&<button type="button" className="icon-button" aria-label="Clear Explorer search" onClick={()=>setQuery('')}><Icon name="close" size={13}/></button>}</div>
    <SidebarSections sectionMode loading={indexing} area="left" group={{kind:'group',id:prefix,tabs:order,active:order[0]}} tabs={tabs} hosts={hosts} expanded={search?[...new Set([...expanded,ids.components,ids.objects])]:expanded} weights={weights} onToggle={id=>{
      const name=id.slice(prefix.length+1);
      setSections(current=>({...current,expanded:current.expanded.includes(name)?current.expanded.filter(value=>value!==name):[...current.expanded,name]}));
    }} onDrop={()=>{}} onResize={next=>{
      const changes=Object.fromEntries(Object.entries(next).map(([id,weight])=>[id.slice(prefix.length+1),weight]));
      setSections(current=>({...current,weights:{...current.weights,...changes}}));
    }}/>
    {Object.entries(contents).map(([id,content])=>createPortal(content,hosts[id],id))}
  </div>;
}
