import React,{useEffect,useState} from 'react';
import type {SceneModel} from '@cadpad/viewer';
import {Icon} from './ui';

export function ProjectExplorer({workspace,parts,assets,parameters,selected,hidden,onSelect,onToggleVisibility,onShowAll}:{
  workspace:string;parameters?:React.ReactNode;parts:SceneModel['parts'];assets:Record<string,string>;selected?:string;
  hidden:Set<string>;onSelect:(id:string)=>void;onToggleVisibility:(id:string)=>void;onShowAll:()=>void;
}){
  const [query,setQuery]=useState('');
  useEffect(()=>setQuery(''),[workspace]);
  const search=query.trim().toLocaleLowerCase();
  const matches=(name:string)=>name.toLocaleLowerCase().includes(search);
  const visibleParts=parts.filter(part=>matches(part.name));
  const assetNames=Object.keys(assets).sort((a,b)=>a.localeCompare(b));
  const matchingAssets=assetNames.filter(matches);
  return <div className="project-explorer">
    {!!(parts.length||assetNames.length)&&<input className="explorer-search" type="search" aria-label="Find parts or assets" placeholder="Find parts or assets…" value={query} onChange={event=>setQuery(event.target.value)}/>}
    <details className="explorer-section" open><summary>Parts <span>{visibleParts.length}</span></summary>
      {!!hidden.size&&<button className="explorer-show-all" onClick={onShowAll}>Show all parts</button>}
      <ul className="explorer-parts" aria-label="Model parts">{visibleParts.map(part=><li className={`part-row ${selected===part.instance_id?'selected':''} ${hidden.has(part.instance_id)?'is-hidden':''}`} key={part.instance_id}>
        <button className="part-select" aria-pressed={selected===part.instance_id} onClick={()=>onSelect(part.instance_id)}><Icon name="cube" size={17}/><span className="explorer-part-caption"><span>{part.name}</span><small>{part.accuracy==='exact'?'CAD geometry':'Mesh reference'}{part.source?.file?` · ${part.source.file}`:''}</small></span></button>
        <button className="visibility-toggle" aria-label={`${hidden.has(part.instance_id)?'Show':'Hide'} ${part.name}`} title={hidden.has(part.instance_id)?'Show part':'Hide part'} onClick={()=>onToggleVisibility(part.instance_id)}><Icon name={hidden.has(part.instance_id)?'eyeOff':'eye'} size={17}/></button>
      </li>)}</ul>
      {!parts.length&&<p className="muted">Ask the assistant to create a part, or import a model.</p>}
    </details>
    {!!React.Children.count(parameters)&&<details className="explorer-section explorer-parameters" open><summary>Parameters <span>{React.Children.count(parameters)}</span></summary>{parameters}</details>}
    {!!assetNames.length&&<details className="explorer-section"><summary>Source assets <span>{matchingAssets.length}</span></summary><ul className="explorer-assets">{matchingAssets.map(path=><li key={path} title={path}><Icon name="file" size={15}/><span>{path.split('/').at(-1)}</span><small>{path.includes('.')?path.split('.').at(-1)?.toUpperCase():'File'}</small></li>)}</ul></details>}
    {!!search&&!visibleParts.length&&!matchingAssets.length&&<p className="muted" role="status">No matching parts or assets.</p>}
  </div>;
}
