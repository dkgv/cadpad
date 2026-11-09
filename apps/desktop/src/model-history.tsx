import React,{useEffect,useRef,useState} from 'react';
import {Icon} from './ui';
type Entry={label?:string;revision:string;restored:boolean;initial:boolean;actor:string;parts:number;created_at?:number};
function historyDay(timestamp?:number){
  if(!timestamp)return 'Earlier';
  const date=new Date(timestamp),today=new Date(),yesterday=new Date();
  yesterday.setDate(today.getDate()-1);
  if(date.toDateString()===today.toDateString())return 'Today';
  if(date.toDateString()===yesterday.toDateString())return 'Yesterday';
  return date.toLocaleDateString(undefined,{month:'long',day:'numeric',...(date.getFullYear()!==today.getFullYear()?{year:'numeric' as const}:{})});
}
export function ModelHistory({entries,disabled,onUndo,onRestore}:{entries:Entry[];disabled:boolean;onUndo:()=>void;onRestore:(revision:string)=>void}){
  const [open,setOpen]=useState(false),root=useRef<HTMLDivElement>(null),trigger=useRef<HTMLButtonElement>(null);
  useEffect(()=>{
    if(!open)return;
    const close=(event:PointerEvent)=>{if(!root.current?.contains(event.target as Node))setOpen(false);};
    document.addEventListener('pointerdown',close);
    return()=>document.removeEventListener('pointerdown',close);
  },[open]);
  return <div className="revision-control" ref={root} onKeyDown={event=>{if(event.key==='Escape'){setOpen(false);trigger.current?.focus();}}}>
    <button className="revision-undo" aria-label="Undo last model change" title="Undo last model change" disabled={disabled||entries.length<2} onClick={onUndo}><Icon name="undo" size={16}/></button>
    <button ref={trigger} className="revision-trigger" aria-label="Model history" title="Model history" aria-expanded={open} aria-haspopup="dialog" onClick={()=>setOpen(!open)}><Icon name="history" size={15}/><span>History</span><Icon name="chevron" size={11}/></button>
    {open&&<section className="revision-popover" role="dialog" aria-label="Model history"><header><strong>Model history</strong><span>{entries.length} versions</span></header><div className="revision-list">{entries.map((entry,index)=><React.Fragment key={entry.revision}>{(index===0||historyDay(entry.created_at)!==historyDay(entries[index-1].created_at))&&<h3 className="revision-day">{historyDay(entry.created_at)}</h3>}<button key={entry.revision} disabled={disabled||index===0} onClick={()=>{setOpen(false);onRestore(entry.revision);}}><i className={index===0?'current':''}/><span><strong>{entry.label||(entry.initial?'Initial model':entry.restored?'Restored version':'Model change')}</strong><small>{entry.created_at?new Date(entry.created_at).toLocaleTimeString(undefined,{hour:'2-digit',minute:'2-digit'}):'Project created'} · {entry.parts} {entry.parts===1?'part':'parts'}</small></span>{index===0?<em>Current</em>:<Icon name="undo" size={13}/>}</button></React.Fragment>)}</div><footer>Select a version to restore it.</footer></section>}
  </div>;
}
