import React,{useEffect,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import {invoke} from '@tauri-apps/api/core';
import {CadViewport,type SceneModel} from '@cadpad/viewer';
import './requirements.css';

type Move={op:'rotate'|'translate';parts:string[];axis?:'x'|'y'|'z'|[number,number,number];origin?:[number,number,number];angle_deg?:number;vector?:[number,number,number]};
export type RequirementReplay={
  requirement_id:string;primitive_id:string;state_index:number;model:{entry:string;builder:string};
  state:unknown;mode:'rest'|'rigid'|'parameter'|'simulation';moves:Move[];
  simulation_poses?:Record<string,{position:[number,number,number];quaternion:[number,number,number,number]}>;
  stateLabel?:string;pairLabel?:string;
  view:{workspace:string;revision:string;temporary:boolean}|null;
};
type ReplayProject={workspace_id:string;revision:string;model:SceneModel};

/** A separate, disposable canvas: replay geometry and poses never enter the model tab. */
export function RequirementReplayPanel({replay,onClose}:{replay:RequirementReplay;onClose:()=>void}){
  const canvas=useRef<HTMLDivElement>(null),[project,setProject]=useState<ReplayProject|null>(null),[error,setError]=useState(''),[loading,setLoading]=useState(true);
  useEffect(()=>{
    let active=true;
    if(!replay.view){setError('The replay view is unavailable.');setLoading(false);return()=>{active=false;};}
    void invoke<ReplayProject>('project',{workspace:replay.view.workspace}).then(value=>{
      if(!active)return;
      if(value.revision!==replay.view!.revision)throw new Error('The replay view expired. Run the requirement again.');
      setProject(value);
    }).catch(value=>{if(active){setError(value instanceof Error?value.message:String(value));setLoading(false);}});
    return()=>{active=false;};
  },[replay.view?.workspace,replay.view?.revision]);
  useEffect(()=>{
    if(!canvas.current||!project)return;
    let active=true;
    let viewport:CadViewport|undefined;
    try{viewport=new CadViewport(canvas.current,()=>{},()=>{},()=>{});}
    catch(value){setError(value instanceof Error?value.message:String(value));setLoading(false);return()=>{active=false;};}
    void viewport.load(project.model,project.revision,handle=>invoke<ArrayBuffer>('artifact',{
      workspace:project.workspace_id,revision:project.revision,handle,
    }),project.workspace_id).then(()=>{
      if(!active)return;
      if(replay.mode==='simulation')viewport!.applyRequirementPoses(replay.simulation_poses??{});
      else viewport!.applyRequirementMoves(replay.moves);
      setLoading(false);
    }).catch(value=>{if(active){setError(value instanceof Error?value.message:String(value));setLoading(false);}});
    return()=>{active=false;viewport?.dispose();};
  },[project,replay.moves]);
  useEffect(()=>()=>{
    if(replay.view?.temporary)void invoke('close_requirement_replay',{workspace:replay.view.workspace}).catch(()=>{});
  },[replay.view?.workspace,replay.view?.temporary]);
  const closeButton=useRef<HTMLButtonElement>(null);
  const close=useRef(onClose);close.current=onClose;
  useEffect(()=>{
    const previous=document.activeElement as HTMLElement|null;
    closeButton.current?.focus();
    const keydown=(event:KeyboardEvent)=>{if(event.key==='Escape'){event.preventDefault();close.current();}};
    window.addEventListener('keydown',keydown);
    return()=>{window.removeEventListener('keydown',keydown);previous?.focus();};
  },[replay.requirement_id,replay.primitive_id,replay.state_index]);
  return createPortal(<div className="rq-replay-backdrop" role="presentation" onMouseDown={event=>{if(event.target===event.currentTarget)onClose();}}>
    <section className="rq-replay" role="dialog" aria-modal="true" aria-label="Worst-state replay">
      <header><div><strong>Worst-state replay</strong><span className="muted">{replay.stateLabel|| (replay.mode==='parameter'?'Rebuilt parameter state':replay.mode==='simulation'?'Simulation sample':replay.mode==='rigid'?'Rigid movement':'As built')}{replay.pairLabel?` · ${replay.pairLabel}`:''}</span></div><button ref={closeButton} type="button" onClick={onClose} aria-label="Close replay">Close</button></header>
      <div ref={canvas} className="rq-replay-canvas" aria-label="Temporary replay viewer"/>
      {loading&&<p className="rq-replay-status" role="status">Preparing replay…</p>}
      {error&&<p className="rq-replay-error" role="alert">{error}</p>}
    </section>
  </div>,document.body);
}
