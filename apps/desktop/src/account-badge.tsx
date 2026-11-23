import React, {useEffect, useRef, useState} from 'react';

/** `GET /v1/account` as returned by the `cloud_account` command. */
export type CloudAccount={email?:string;name?:string;plan?:string;remaining?:number;generation_cost?:number};

function initials(account:CloudAccount|null){
  const name=account?.name?.trim();
  if(name&&!name.includes('@')){
    const words=name.split(/\s+/).filter(Boolean);
    return (words[0][0]+(words.length>1?words[words.length-1][0]:'')).toUpperCase();
  }
  const email=account?.email?.trim()||name;
  return email?email[0].toUpperCase():'';
}

/** Signed-in Cadpad Cloud user, pinned to the bottom of the activity rail. */
export function AccountBadge({account,onSignOut,onSettings}:{account:CloudAccount|null;onSignOut:()=>void;onSettings:()=>void}){
  const [open,setOpen]=useState(false);
  const root=useRef<HTMLDivElement>(null);
  useEffect(()=>{
    if(!open)return;
    const outside=(event:PointerEvent)=>{if(!root.current?.contains(event.target as Node))setOpen(false);};
    const escape=(event:KeyboardEvent)=>{if(event.key==='Escape'){setOpen(false);root.current?.querySelector('button')?.focus();}};
    document.addEventListener('pointerdown',outside,true);document.addEventListener('keydown',escape);
    return()=>{document.removeEventListener('pointerdown',outside,true);document.removeEventListener('keydown',escape);};
  },[open]);
  const label=account?.name||account?.email||'Cadpad Cloud account';
  const letters=initials(account);
  const plan=account?.plan?account.plan[0].toUpperCase()+account.plan.slice(1):'Free';
  return <div className="account-badge" ref={root}>
    <button type="button" className="account-avatar" aria-label={`Account: ${label}`} data-sidebar-tooltip={open?undefined:label} aria-haspopup="dialog" aria-expanded={open} onClick={()=>setOpen(!open)}>
      <span aria-hidden="true">{letters||<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round"><circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/></svg>}</span>
    </button>
    {open&&<div className="account-popover" role="dialog" aria-label="Cadpad Cloud account">
      <div className="account-identity">
        <span className="account-avatar-large" aria-hidden="true">{letters||'·'}</span>
        <div>{account?.name&&account.name!==account.email&&<strong>{account.name}</strong>}<span>{account?.email||'Signed in to Cadpad Cloud'}</span></div>
      </div>
      {typeof account?.remaining==='number'&&<p className="account-credits"><span>{plan} plan</span><span>{account.remaining.toLocaleString()} credit{account.remaining===1?'':'s'} left</span></p>}
      <div className="account-actions">
        <button type="button" onClick={()=>{setOpen(false);onSettings();}}>Settings…</button>
        <button type="button" className="danger-button" onClick={()=>{setOpen(false);onSignOut();}}>Sign out</button>
      </div>
    </div>}
  </div>;
}
