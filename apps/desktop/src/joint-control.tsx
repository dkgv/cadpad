import React,{useEffect,useRef,useState} from 'react';
import type {MotionSnapshot} from '@cadpad/viewer';

type Joint=MotionSnapshot['joints'][number];
export function JointControl({joint,name,coordinate,disabled,motor,onLocate,onPreview,onCommit}:{joint:Joint;name:string;coordinate:number;disabled:boolean;motor:boolean;onLocate?:()=>void;onPreview:(value:number)=>void;onCommit:(value:number)=>void}){
  const factor=joint.unit==='m'?1000:joint.unit==='rad'?180/Math.PI:1;
  const unit=joint.unit==='m'?'mm':joint.unit==='rad'?'°':joint.unit;
  const format=(value:number)=>Number((value*factor).toFixed(3)).toString();
  const [text,setText]=useState(format(coordinate)),[editing,setEditing]=useState(false);
  useEffect(()=>{if(!editing)setText(format(coordinate));},[coordinate,factor,editing]);
  const preview=useRef(coordinate);preview.current=coordinate;
  const commit=()=>{
    const value=Number(text);
    if(text.trim()&&Number.isFinite(value)){
      const target=joint.limits?Math.max(joint.limits[0],Math.min(joint.limits[1],value/factor)):value/factor;
      onCommit(target);setText(format(target));
    }else setText(format(coordinate));
    setEditing(false);
  };
  return <div className="motion-control">
    <div className="joint-heading"><div><strong>{onLocate?<button className="joint-part-name" aria-label={`Locate ${name} in canvas`} onClick={onLocate}>{name}</button>:name}</strong><small className="joint-description">{motor?'Motor target':joint.kind==='revolute'?'Rotate':joint.kind==='path'?'Follow track':'Slide'}</small></div><label className="joint-value"><span className="visually-hidden">{name} {motor?'motor target':'position'}</span><input type="number" aria-label={`${joint.id} position`} disabled={disabled} value={text} min={joint.limits?joint.limits[0]*factor:undefined} max={joint.limits?joint.limits[1]*factor:undefined} step="any" onFocus={()=>setEditing(true)} onChange={event=>setText(event.target.value)} onBlur={commit} onKeyDown={event=>{if(event.key==='Enter')event.currentTarget.blur();if(event.key==='Escape'){setText(format(coordinate));setEditing(false);}}}/><span>{unit}</span></label></div>
    {joint.limits?<><input aria-label={joint.id} aria-valuetext={`${format(coordinate)} ${unit}`} disabled={disabled} type="range" min={joint.limits[0]*factor} max={joint.limits[1]*factor} step={(joint.limits[1]-joint.limits[0])*factor/1000||0.001} value={coordinate*factor} onChange={event=>{preview.current=Number(event.target.value)/factor;onPreview(preview.current);}} onPointerUp={()=>onCommit(preview.current)} onPointerCancel={()=>onCommit(preview.current)}/><small className="joint-range"><span>{format(joint.limits[0])} {unit}</span><span>{format(joint.limits[1])} {unit}</span></small></>:<small className="muted">Enter a position above. This joint has no travel limits.</small>}
  </div>;
}
