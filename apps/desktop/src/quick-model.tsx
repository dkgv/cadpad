import React,{useEffect,useId,useMemo,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import {Icon,errorText} from './ui';
import {indexedModels,loadSourceIndex,modelCaption,useSourceCatalogEpoch,type IndexedModel} from './source-catalog';

const recentKey=(workspace:string)=>`cadpad.recent-models.v1:${workspace}`;
const modelKey=(entry:string,builder:string)=>JSON.stringify([entry,builder]);

function recentModels(workspace:string):string[]{
  try{
    const stored:unknown=JSON.parse(window.localStorage.getItem(recentKey(workspace))??'[]');
    return Array.isArray(stored)?stored.filter((key):key is string=>typeof key==='string'):[];
  }catch{return [];}
}

export function rememberRecentModel(workspace:string,entry:string,builder:string){
  const key=modelKey(entry,builder);
  const recent=[key,...recentModels(workspace).filter(item=>item!==key)].slice(0,32);
  try{window.localStorage.setItem(recentKey(workspace),JSON.stringify(recent));}catch{}
}

function normalizeSearch(value:string){
  return value.toLowerCase().replace(/[_\s]+/g,' ');
}

function fuzzyGap(query:string,value:string){
  let end=0,gap=0;
  for(const character of query){
    const index=value.indexOf(character,end);
    if(index===-1)return Infinity;
    if(end>0)gap+=index-end;
    end=index+character.length;
  }
  return gap;
}

function searchRank(query:string,value:string){
  const field=normalizeSearch(value);
  if(field===query)return {tier:0,gap:0};
  if(field.startsWith(query))return {tier:1,gap:0};
  if(field.includes(query))return {tier:2,gap:0};
  return {tier:3,gap:fuzzyGap(query,field)};
}

function compareRank(a:{tier:number;gap:number},b:{tier:number;gap:number}){
  return a.tier-b.tier||a.gap-b.gap;
}

export function QuickModelSelector({open,workspace,revision,historyWorkspace=workspace,activeEntry,activeBuilder,busy,onClose,onChoose}:{
  open:boolean;workspace:string;revision:string;historyWorkspace?:string;activeEntry?:string;activeBuilder?:string;busy?:boolean;onClose:()=>void;onChoose:(path:string,builder:string)=>void;
}){
  const [query,setQuery]=useState(''),[highlight,setHighlight]=useState(0),[loading,setLoading]=useState(false),[error,setError]=useState('');
  const [models,setModels]=useState<IndexedModel[]>([]);
  const popup=useRef<HTMLDivElement>(null),input=useRef<HTMLInputElement>(null);
  const close=useRef(onClose);close.current=onClose;
  const choose=useRef(onChoose);choose.current=onChoose;
  const id=useId();
  const catalogEpoch=useSourceCatalogEpoch();
  const search=normalizeSearch(query.trim());
  const recentRanks=useMemo(()=>new Map((open?recentModels(historyWorkspace):[]).map((key,index)=>[key,index])),[open,historyWorkspace]);
  const matches=useMemo(()=>{
    if(!search){
      if(!recentRanks.size)return models;
      const rank=({file,builder}:IndexedModel)=>recentRanks.get(modelKey(file.path,builder.id))??recentRanks.size;
      return [...models].sort((a,b)=>rank(a)-rank(b));
    }
    const ranked:{model:IndexedModel;tier:number;gap:number}[]=[];
    for(const model of models){
      const {file,builder}=model;
      let rank=searchRank(search,modelCaption(builder));
      for(const field of [builder.label,file.path,file.description]){
        const candidate=searchRank(search,field);
        // A shared file match should rank below a match in the displayed model name.
        candidate.tier+=4;
        if(Number.isFinite(candidate.gap)&&(!Number.isFinite(rank.gap)||compareRank(candidate,rank)<0))rank=candidate;
      }
      if(Number.isFinite(rank.gap))ranked.push({model,...rank});
    }
    return ranked.sort(compareRank).map(({model})=>model);
  },[models,search,recentRanks]);
  useEffect(()=>{
    if(!open)return;
    let active=true;setQuery('');setHighlight(0);setLoading(true);setError('');setModels([]);
    loadSourceIndex(undefined,workspace,revision).then(result=>{if(active)setModels(indexedModels(result.files));})
      .catch(value=>{if(active)setError(errorText(value));}).finally(()=>{if(active)setLoading(false);});
    return()=>{active=false;};
  },[open,workspace,revision,catalogEpoch]);
  useEffect(()=>{if(open)setHighlight(0);},[query,open]);
  useEffect(()=>{
    if(!open)return;
    input.current?.focus();
    const outside=(event:PointerEvent)=>{if(!popup.current?.contains(event.target as Node))close.current();};
    const escape=(event:KeyboardEvent)=>{if(event.key==='Escape'){event.preventDefault();event.stopPropagation();close.current();}};
    document.addEventListener('pointerdown',outside,true);document.addEventListener('keydown',escape,true);
    return()=>{document.removeEventListener('pointerdown',outside,true);document.removeEventListener('keydown',escape,true);};
  },[open]);
  useEffect(()=>{
    if(!open)return;
    const option=popup.current?.querySelector(`[data-result-index="${highlight}"]`) as HTMLElement|null;
    option?.scrollIntoView?.({block:'nearest'});
  },[highlight,open,query,matches.length]);
  function select(path:string,builder:string){if(busy)return;choose.current(path,builder);close.current();}
  if(!open)return null;
  return createPortal(<div className="quick-model-backdrop" role="presentation">
    <div ref={popup} id={id} role="dialog" aria-label="Quick model selector" className="quick-model-popover" onKeyDown={event=>{
      if(event.key==='ArrowDown'||event.key==='ArrowUp'){
        event.preventDefault();setHighlight(current=>matches.length?(current+(event.key==='ArrowDown'?1:-1)+matches.length)%matches.length:0);input.current?.focus();
      }else if(event.key==='Enter'&&event.target===input.current){event.preventDefault();const match=matches[highlight];if(match)select(match.file.path,match.builder.id);}
    }}>
      <div className="quick-model-search"><Icon name="search" size={18}/><input ref={input} type="search" autoCorrect="off" autoComplete="off" autoCapitalize="off" spellCheck={false} aria-label="Search models" placeholder="Go to model…" value={query} onChange={event=>setQuery(event.target.value)} role="combobox" aria-autocomplete="list" aria-expanded="true" aria-controls={`${id}-results`} aria-activedescendant={matches[highlight]?`${id}-option-${highlight}`:undefined}/></div>
      <div className="quick-model-results" role="listbox" id={`${id}-results`} aria-label="Project models" aria-busy={loading||!!busy}>
        {loading&&<p className="quick-model-empty" role="status">Indexing project models…</p>}
        {error&&<p className="quick-model-empty" role="alert">{error}</p>}
        {!loading&&!error&&matches.map(({file,builder},index)=>{
          const current=activeEntry===file.path&&activeBuilder===builder.id;
          return <button key={`${file.path}:${builder.id}`} id={`${id}-option-${index}`} data-result-index={index} type="button" role="option" aria-selected={current} disabled={busy} className={`quick-model-row ${index===highlight?'highlighted':''}`} title={`${file.path}:${builder.line} · ${builder.label}`} onPointerMove={()=>setHighlight(index)} onClick={()=>select(file.path,builder.id)}>
            <Icon name="cube" size={15}/>
            <span className="quick-model-copy"><strong>{modelCaption(builder)}</strong><small>{file.path}</small></span>
            {current&&<span className="quick-model-current">Open</span>}
          </button>;
        })}
        {!loading&&!error&&!matches.length&&<p className="quick-model-empty" role="status">{search?'No matching models':'No models found in this project'}</p>}
      </div>
    </div>
  </div>,document.body);
}
