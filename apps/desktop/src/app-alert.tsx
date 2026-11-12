import React,{useEffect,useId,useRef,useState} from 'react';
import {Icon,errorSummary} from './ui';

export function AppAlert({message,active=true,onDismiss}:{message:string;active?:boolean;onDismiss:()=>void}){
  const [minimized,setMinimized]=useState(true);
  const dialog=useRef<HTMLDialogElement>(null),restore=useRef<HTMLButtonElement>(null),restoreFocus=useRef(false),title=useId(),description=useId();
  const minimize=()=>{restoreFocus.current=true;setMinimized(true);};
  useEffect(()=>{
    const node=dialog.current;if(!node)return;
    if(active&&!minimized){if(!node.open)node.showModal();}
    else if(node.open)node.close();
    if(active&&minimized&&restoreFocus.current){restore.current?.focus();restoreFocus.current=false;}
    return()=>{if(node.open)node.close();};
  },[active,minimized]);
  const summary=errorSummary(message);
  return <>
    <dialog ref={dialog} className="app-alert-dialog" aria-labelledby={title} aria-describedby={description} onCancel={event=>{event.preventDefault();minimize();}}>
      <header><h2 id={title}>Action needs attention</h2><div><button type="button" className="icon-button" aria-label="Minimize alert" title="Minimize" onClick={minimize}><span aria-hidden="true">−</span></button><button type="button" className="icon-button" aria-label="Dismiss alert" onClick={onDismiss}><Icon name="close" size={16}/></button></div></header>
      <div className="app-alert-body"><p id={description}>{summary}</p><details><summary>Technical details</summary><pre>{message}</pre></details></div>
      <footer><button type="button" onClick={minimize}>Keep working</button><button type="button" className="primary-button" onClick={onDismiss}>Dismiss</button></footer>
    </dialog>
    {active&&minimized&&<div className="app-alert-toast" role="status"><button ref={restore} type="button" className="app-alert-open" aria-label="Review alert" aria-haspopup="dialog" onClick={()=>setMinimized(false)}><span className="app-alert-dot" aria-hidden="true"/><span><strong>Action needs attention</strong><small>{summary}</small></span></button><button type="button" className="icon-button" aria-label="Dismiss notification" onClick={onDismiss}><Icon name="close" size={14}/></button></div>}
  </>;
}
