import React,{useEffect,useId,useLayoutEffect,useMemo,useRef,useState} from 'react';
import {Icon} from './ui';

export type ModelOption={value:string;label:string;detail?:string};

type Props={
  ariaLabel:string;
  value:string;
  options:ModelOption[];
  placeholder:string;
  className?:string;
  disabled?:boolean;
  loading?:boolean;
  onChange:(value:string)=>void;
};

/**
 * Searchable model picker. The panel is fixed-positioned so it can escape the
 * composer's clipping and the settings sheet's scroll area.
 */
export function SearchableModelSelect({ariaLabel,value,options,placeholder,className='',disabled=false,loading=false,onChange}:Props){
  const [open,setOpen]=useState(false);
  const [query,setQuery]=useState('');
  const [highlight,setHighlight]=useState(0);
  const [placement,setPlacement]=useState({left:0,top:0,width:260,maxHeight:320});
  const wrapper=useRef<HTMLDivElement>(null);
  const trigger=useRef<HTMLButtonElement>(null);
  const search=useRef<HTMLInputElement>(null);
  const id=useId();
  const selected=options.find(option=>option.value===value);
  const normalized=query.trim().toLowerCase();
  const matches=useMemo(()=>{
    if(!normalized)return options;
    const ranked:{option:ModelOption;tier:number;index:number}[]=[];
    options.forEach((option,index)=>{
      const label=option.label.toLowerCase();
      const model=option.value.toLowerCase();
      const tier=label===normalized||model===normalized?0
        :model.startsWith(normalized)||label.startsWith(normalized)?1
        :label.includes(normalized)||model.includes(normalized)?2:3;
      if(tier<3)ranked.push({option,tier,index});
    });
    return ranked.sort((left,right)=>left.tier-right.tier||left.index-right.index).map(item=>item.option);
  },[options,normalized]);

  function close(refocus:boolean){
    setOpen(false);
    setQuery('');
    if(refocus)requestAnimationFrame(()=>trigger.current?.focus());
  }
  function openPicker(){
    if(disabled)return;
    const index=options.findIndex(option=>option.value===value);
    setHighlight(index<0?0:index);
    setQuery('');
    setOpen(true);
  }
  function choose(option:ModelOption){
    onChange(option.value);
    close(true);
  }

  useEffect(()=>{
    if(!open)return;
    requestAnimationFrame(()=>search.current?.focus());
    const outside=(event:PointerEvent)=>{if(!wrapper.current?.contains(event.target as Node))close(false);};
    const escape=(event:KeyboardEvent)=>{
      if(event.key!=='Escape')return;
      event.preventDefault();
      event.stopPropagation();
      close(true);
    };
    const focus=(event:FocusEvent)=>{
      if(!wrapper.current?.contains(event.target as Node))close(false);
    };
    document.addEventListener('pointerdown',outside,true);
    document.addEventListener('keydown',escape,true);
    document.addEventListener('focusin',focus,true);
    return()=>{
      document.removeEventListener('pointerdown',outside,true);
      document.removeEventListener('keydown',escape,true);
      document.removeEventListener('focusin',focus,true);
    };
  },[open]);

  useLayoutEffect(()=>{
    if(!open)return;
    const place=()=>{
      const anchor=trigger.current;
      if(!anchor)return;
      const rect=anchor.getBoundingClientRect();
      const width=Math.min(Math.max(rect.width,260),Math.max(240,window.innerWidth-16));
      const below=window.innerHeight-rect.bottom-8;
      const above=rect.top-8;
      const openAbove=below<180&&above>below;
      const maxHeight=Math.max(140,Math.min(320,(openAbove?above:below)-6));
      const left=Math.min(Math.max(8,rect.left),Math.max(8,window.innerWidth-width-8));
      const top=openAbove?Math.max(8,rect.top-maxHeight-6):Math.min(rect.bottom+6,Math.max(8,window.innerHeight-maxHeight-8));
      setPlacement({left,top,width,maxHeight});
    };
    place();
    window.addEventListener('resize',place);
    window.addEventListener('scroll',place,true);
    return()=>{
      window.removeEventListener('resize',place);
      window.removeEventListener('scroll',place,true);
    };
  },[open]);

  function searchKeyDown(event:React.KeyboardEvent<HTMLInputElement>){
    if(event.key==='ArrowDown'||event.key==='ArrowUp'){
      event.preventDefault();
      if(!matches.length)return;
      setHighlight(current=>(current+(event.key==='ArrowDown'?1:-1)+matches.length)%matches.length);
    }else if(event.key==='Enter'){
      event.preventDefault();
      const match=matches[highlight];
      if(match)choose(match);
    }else if(event.key==='Home'||event.key==='End'){
      event.preventDefault();
      setHighlight(event.key==='Home'?0:Math.max(0,matches.length-1));
    }
  }

  return <div className="searchable-select" ref={wrapper}>
    <button ref={trigger} type="button" className={`searchable-select-trigger ${className}`.trim()} aria-label={ariaLabel} aria-haspopup="listbox" aria-expanded={open} data-value={value} title={selected?.label||placeholder} disabled={disabled} onClick={()=>open?close(true):openPicker()} onKeyDown={event=>{if(event.key==='ArrowDown'){event.preventDefault();openPicker();}}}>
      <span className="searchable-select-label">{selected?.label||placeholder}</span>
      <Icon name="chevron" size={12}/>
    </button>
    {open&&<div className="searchable-select-panel" style={placement}>
      <div className="searchable-select-search"><Icon name="search" size={14}/><input ref={search} type="search" autoCorrect="off" autoComplete="off" autoCapitalize="off" spellCheck={false} aria-label="Search models" placeholder={`Search ${ariaLabel.toLowerCase()}…`} value={query} onChange={event=>{setQuery(event.target.value);setHighlight(0);}} onKeyDown={searchKeyDown} role="combobox" aria-autocomplete="list" aria-expanded="true" aria-controls={`${id}-options`} aria-activedescendant={matches[highlight]?`${id}-option-${highlight}`:undefined}/></div>
      <div className="searchable-select-list" role="listbox" id={`${id}-options`} aria-label={`${ariaLabel} options`}>
        {loading&&<p className="searchable-select-empty" role="status">Loading models…</p>}
        {matches.map((option,index)=><button key={option.value} id={`${id}-option-${index}`} type="button" role="option" aria-selected={option.value===value} tabIndex={-1} data-value={option.value} className={`searchable-select-option${index===highlight?' highlighted':''}`} onPointerMove={()=>setHighlight(index)} onClick={()=>choose(option)}>
          <strong>{option.label}</strong>
          {!!option.detail&&option.detail!==option.label&&<small>{option.detail}</small>}
        </button>)}
        {!loading&&!matches.length&&<p className="searchable-select-empty" role="status">{normalized?'No matching models':'No models available'}</p>}
      </div>
    </div>}
  </div>;
}
