import React,{useEffect,useId,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import {Icon} from './ui';
import type {ChatSummary} from './assistant-chats';

export function chatDateGroup(timestamp:number,now=new Date()){
  const today=new Date(now.getFullYear(),now.getMonth(),now.getDate());
  const yesterday=new Date(today);yesterday.setDate(today.getDate()-1);
  const week=new Date(today);week.setDate(today.getDate()-7);
  return timestamp>=today.getTime()?'Today':timestamp>=yesterday.getTime()?'Yesterday':timestamp>=week.getTime()?'Previous 7 days':'Older';
}

export function ChatHistory({chats,activeId,busy,onSelect}:{chats:ChatSummary[];activeId:string|null;busy:boolean;onSelect:(id:string)=>Promise<boolean>}){
  const [open,setOpen]=useState(false),[query,setQuery]=useState(''),[highlight,setHighlight]=useState(0);
  const [position,setPosition]=useState<React.CSSProperties>({left:0,top:0,width:320,maxHeight:480});
  const trigger=useRef<HTMLButtonElement>(null),popup=useRef<HTMLDivElement>(null),input=useRef<HTMLInputElement>(null);
  const id=useId();
  const matches=chats.filter(chat=>chat.message_count>0&&chat.title.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())).sort((a,b)=>b.updated_at-a.updated_at||a.id.localeCompare(b.id));
  const indices=new Map(matches.map((chat,index)=>[chat.id,index]));
  const sections=['Today','Yesterday','Previous 7 days','Older'].map(label=>({label,chats:matches.filter(chat=>chatDateGroup(chat.updated_at)===label)})).filter(section=>section.chats.length);
  function close(restoreFocus=false){setOpen(false);if(restoreFocus)trigger.current?.focus();}
  function show(){
    if(open){close();return;}
    const rect=trigger.current!.getBoundingClientRect(),width=Math.min(340,window.innerWidth-24);
    const below=window.innerHeight-rect.bottom-12,above=rect.top-12,down=below>=Math.min(420,above);
    const maxHeight=Math.max(80,Math.min(480,down?below:above));
    setPosition({left:Math.max(12,Math.min(rect.right-width,window.innerWidth-width-12)),top:down?rect.bottom+6:undefined,bottom:down?undefined:window.innerHeight-rect.top+6,width,maxHeight});
    setQuery('');setHighlight(0);setOpen(true);
  }
  useEffect(()=>{
    if(!open)return;
    input.current?.focus();
    const outside=(event:PointerEvent)=>{if(!popup.current?.contains(event.target as Node)&&!trigger.current?.contains(event.target as Node))close();};
    const escape=(event:KeyboardEvent)=>{if(event.key==='Escape'){event.preventDefault();event.stopPropagation();close(true);}};
    const dismiss=()=>close();
    document.addEventListener('pointerdown',outside,true);document.addEventListener('keydown',escape,true);window.addEventListener('resize',dismiss);
    return()=>{document.removeEventListener('pointerdown',outside,true);document.removeEventListener('keydown',escape,true);window.removeEventListener('resize',dismiss);};
  },[open]);
  useEffect(()=>{if(open)popup.current?.querySelector(`[data-result-index="${highlight}"]`)?.scrollIntoView({block:'nearest'});},[highlight,open,query]);
  async function select(chatId:string){if(!busy&&await onSelect(chatId))close();}
  return <>
    <button ref={trigger} type="button" className="icon-button chat-history-trigger" title="Chat history" aria-label="Chat history" aria-haspopup="dialog" aria-expanded={open} aria-controls={open?id:undefined} onClick={show}><Icon name="history" size={15}/></button>
    {open&&createPortal(<div ref={popup} id={id} role="dialog" aria-label="Chat history" className="chat-history-popover" style={position} onKeyDown={event=>{
      if(event.key==='ArrowDown'||event.key==='ArrowUp'){
        event.preventDefault();setHighlight(current=>matches.length?(current+(event.key==='ArrowDown'?1:-1)+matches.length)%matches.length:0);input.current?.focus();
      }else if(event.key==='Enter'&&event.target===input.current){event.preventDefault();if(matches[highlight])void select(matches[highlight].id);}
    }}>
      <div className="chat-history-search"><Icon name="search" size={18}/><input ref={input} type="search" aria-label="Search chats" placeholder="Search chats…" value={query} onChange={event=>{setQuery(event.target.value);setHighlight(0);}} role="combobox" aria-autocomplete="list" aria-expanded="true" aria-controls={`${id}-results`} aria-activedescendant={matches[highlight]?`${id}-option-${highlight}`:undefined}/></div>
      <div className="chat-history-results" role="listbox" id={`${id}-results`} aria-label="Saved chats" aria-busy={busy}>
        {sections.map(section=><div key={section.label} role="group" aria-label={section.label} className="chat-history-section"><div className="chat-history-date" aria-hidden="true">{section.label}</div>{section.chats.map(chat=>{
          const index=indices.get(chat.id)!;
          return <button key={chat.id} id={`${id}-option-${index}`} data-result-index={index} type="button" role="option" aria-selected={activeId===chat.id} disabled={busy} className={`chat-history-row ${index===highlight?'highlighted':''}`} title={chat.title} onPointerMove={()=>setHighlight(index)} onClick={()=>void select(chat.id)}><span className={`chat-history-marker ${activeId===chat.id?'current':''}`} aria-hidden="true"/><span>{chat.title}</span></button>;
        })}</div>)}
        {!matches.length&&<p className="chat-history-empty" role="status">{query.trim()?'No matching chats':'No saved chats yet'}</p>}
      </div>
    </div>,document.body)}
  </>;
}
