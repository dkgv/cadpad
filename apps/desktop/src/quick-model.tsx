import React,{useEffect,useId,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import {Icon,errorSummary} from './ui';
import {indexedModels,loadSourceIndex,modelCaption,type IndexedModel} from './source-catalog';

export function QuickModelSelector({open,workspace,revision,activeEntry,activeBuilder,busy,onClose,onChoose}:{
  open:boolean;workspace:string;revision:string;activeEntry?:string;activeBuilder?:string;busy?:boolean;onClose:()=>void;onChoose:(path:string,builder:string)=>void;
}){
  const [query,setQuery]=useState(''),[highlight,setHighlight]=useState(0),[loading,setLoading]=useState(false),[error,setError]=useState('');
  const [models,setModels]=useState<IndexedModel[]>([]);
  const popup=useRef<HTMLDivElement>(null),input=useRef<HTMLInputElement>(null);
  const close=useRef(onClose);close.current=onClose;
  const choose=useRef(onChoose);choose.current=onChoose;
  const id=useId();
  const search=query.trim().toLowerCase();
  const matches=models.filter(({file,builder})=>`${modelCaption(builder)} ${builder.label} ${file.path} ${file.description}`.toLowerCase().includes(search));
  useEffect(()=>{
    if(!open)return;
    let active=true;setQuery('');setHighlight(0);setLoading(true);setError('');setModels([]);
    loadSourceIndex(undefined,workspace,revision).then(result=>{if(active)setModels(indexedModels(result.files));})
      .catch(value=>{if(active)setError(errorSummary(JSON.stringify(value)));}).finally(()=>{if(active)setLoading(false);});
    return()=>{active=false;};
  },[open,workspace,revision]);
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
      <div className="quick-model-search"><Icon name="search" size={18}/><input ref={input} type="search" aria-label="Search models" placeholder="Go to model…" value={query} onChange={event=>setQuery(event.target.value)} role="combobox" aria-autocomplete="list" aria-expanded="true" aria-controls={`${id}-results`} aria-activedescendant={matches[highlight]?`${id}-option-${highlight}`:undefined}/></div>
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
