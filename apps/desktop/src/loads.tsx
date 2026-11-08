import React,{useEffect,useState} from 'react';
import type {MotionSnapshot} from '@cadpad/viewer';
import './loads.css';

type Simulation=NonNullable<MotionSnapshot['simulation']>;
type Loads=NonNullable<Simulation['external_loads']>;
type Load=Loads[string];
const zero=():Load=>({force_n:[0,0,0],torque_nm:[0,0,0]});

export function ExternalLoads({simulation,disabled,onApply}:{simulation:Simulation;disabled:boolean;onApply:(loads:Loads)=>void}){
  const bodies=Object.keys(simulation.mass_kg),loads=simulation.external_loads??{};
  const [selected,setSelected]=useState(bodies[0]??'');
  const [draft,setDraft]=useState<Load>(()=>loads[selected]??zero());
  const applied=JSON.stringify(loads[selected]??zero());
  useEffect(()=>{setDraft(JSON.parse(applied));},[selected,applied]);
  const valid=!!selected&&bodies.includes(selected)&&[...draft.force_n,...draft.torque_nm].every(value=>Number.isFinite(value)&&Math.abs(value)<=1e9);
  return <details className="external-loads"><summary>Force and torque</summary>
    <p className="muted">World axes. Force acts at the center of mass. Loads stay active until cleared.</p>
    <label>Body<select aria-label="Load body" value={selected} onChange={event=>setSelected(event.target.value)}>{bodies.map(name=><option key={name} value={name}>{name}</option>)}</select></label>
    {(['force_n','torque_nm'] as const).map(kind=><fieldset key={kind}><legend>{kind==='force_n'?'Force (N)':'Torque (N m)'}</legend><div className="load-components">{(['X','Y','Z'] as const).map((axis,index)=><label key={axis}>{axis}<input aria-label={`${kind==='force_n'?'Force':'Torque'} ${axis} (${kind==='force_n'?'N':'N m'})`} type="number" min={-1e9} max={1e9} step="any" value={Number.isNaN(draft[kind][index])?'':draft[kind][index]} onChange={event=>setDraft({...draft,[kind]:draft[kind].map((value,i)=>i===index?event.target.valueAsNumber:value)})}/></label>)}</div></fieldset>)}
    <div className="load-actions"><button disabled={disabled||!valid} onClick={()=>onApply({...loads,[selected]:draft})}>Apply load</button>
    <button disabled={disabled||!loads[selected]} onClick={()=>{const remaining={...loads};delete remaining[selected];onApply(remaining);}}>Clear body load</button>
    <button disabled={disabled||!Object.keys(loads).length} onClick={()=>onApply({})}>Clear all loads</button></div>
    <p className="muted">{Object.keys(loads).length} bodies with applied loads. Pause simulation to change them.</p>
  </details>;
}
