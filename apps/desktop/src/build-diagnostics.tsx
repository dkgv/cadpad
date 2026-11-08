import React,{useState} from 'react';
import {Icon} from './ui';

export function buildErrorInfo(value:unknown){
  let data=value as any;if(typeof data==='string'){try{data=JSON.parse(data);}catch{data={message:data};}}
  const detail=data?.details?.details||data?.details||data||{};
  const traceback=String(detail.traceback||data?.traceback||'');
  const frames=[...traceback.matchAll(/File "[^"\n]*[/\\]source[/\\]([^"\n]+)", line (\d+)/g)];
  const frame=frames.at(-1),location=frame?`${frame[1]}:${frame[2]}`:'';
  const raw=String(data?.message||detail.message||'The model could not be built.');
  const writes=/Operation not permitted|PermissionError/.test(raw+'\n'+traceback)&&/[/\\]source[/\\]/.test(raw);
  return {summary:writes?'The script tried to write files during the build.':raw,hint:writes?'Set up a Cadpad entry point to display geometry without running the export routine.':'',location,traceback,logs:String(detail.logs||data?.logs||''),raw};
}
export function BuildDiagnostics({diagnostics,onRetry,onSetup}:{diagnostics:unknown;onRetry:()=>void;onSetup?:()=>void}){
  const [open,setOpen]=useState(true),[dismissed,setDismissed]=useState(false);
  if(dismissed)return null;
  const info=buildErrorInfo(diagnostics);
  return <details className="build-diagnostics" open={open} onToggle={event=>setOpen(event.currentTarget.open)} aria-label="Build error">
    <summary><span className="build-failed-dot"/><strong>Build failed</strong>{info.location&&<span className="build-error-location">{info.location}</span>}<button className="icon-button" aria-label="Dismiss build error" title="Dismiss build error" onClick={event=>{event.preventDefault();setDismissed(true);}}><Icon name="close" size={13}/></button></summary>
    <div className="build-error-content"><p role="alert">{info.summary}</p>{info.hint&&<p className="muted">{info.hint}</p>}<div className="build-error-actions">{onSetup&&<button onClick={onSetup}>Set up entry point…</button>}<button onClick={onRetry}>Retry build</button></div><details className="build-error-technical"><summary>Technical details</summary><pre>{info.traceback||info.raw}{info.logs?`\n\nBuild output\n${info.logs}`:''}</pre></details></div>
  </details>;
}
