import {useState} from 'react';

type Vector=[number,number,number];
type Props={density:number;friction:number;bounce:number;gravity:Vector;onDensity:(value:number)=>void;onFriction:(value:number)=>void;onBounce:(value:number)=>void;onGravity:(value:Vector)=>void};
const materials=[{name:'Plastic',density:1000},{name:'Aluminium',density:2700},{name:'Steel',density:7850}];
const grips=[{name:'Slippery',value:0.1},{name:'Some grip',value:0.5},{name:'Grippy',value:0.9}];

export function PhysicsSettings({density,friction,bounce,gravity,onDensity,onFriction,onBounce,onGravity}:Props){
  const [custom,setCustom]=useState(false);
  const gravityMode=gravity.every(value=>value===0)?'off':gravity[0]===0&&gravity[1]===0&&gravity[2]===-9.81?'earth':'custom';
  return <div className="physics-settings">
    <h4>How should the parts behave?</h4>
    <p className="physics-settings-intro">These are starting estimates. Keep the defaults to try the motion, or change them to explore a different behaviour.</p>
    <label htmlFor="physics-material">Moving parts are made of
      <select id="physics-material" value={materials.some(item=>item.density===density)?String(density):'custom'} onChange={event=>event.target.value==='custom'?setCustom(true):onDensity(Number(event.target.value))}>
        {materials.map(item=><option key={item.name} value={item.density}>{item.name}</option>)}<option value="custom">Custom material…</option>
      </select>
      <small>Sets the weight of all parts marked “Can move”. Heavier parts need more force to accelerate.</small>
    </label>
    <label htmlFor="physics-grip">When surfaces touch
      <select id="physics-grip" value={grips.some(item=>item.value===friction)?String(friction):'custom'} onChange={event=>event.target.value==='custom'?setCustom(true):onFriction(Number(event.target.value))}>
        {grips.map(item=><option key={item.name} value={item.value}>{item.name}</option>)}<option value="custom">Custom grip…</option>
      </select>
      <small>Slippery surfaces slide more easily. More grip resists sliding at contacts.</small>
    </label>
    <label htmlFor="physics-bounce">When parts collide
      <select id="physics-bounce" value={[0,0.2,0.6].includes(bounce)?String(bounce):'custom'} onChange={event=>event.target.value==='custom'?setCustom(true):onBounce(Number(event.target.value))}>
        <option value="0">No bounce</option><option value="0.2">A little bounce</option><option value="0.6">Lots of bounce</option><option value="custom">Custom bounce…</option>
      </select>
      <small>Controls how much parts rebound after hitting each other.</small>
    </label>
    <label htmlFor="physics-gravity">Gravity
      <select id="physics-gravity" value={gravityMode} onChange={event=>event.target.value==='custom'?setCustom(true):onGravity(event.target.value==='earth'?[0,0,-9.81]:[0,0,0])}>
        <option value="earth">On — Earth gravity</option><option value="off">Off — weightless</option><option value="custom">Custom direction or strength…</option>
      </select>
      <small>{gravityMode==='earth'?'Pulls movable parts downward along the model’s Z axis.':gravityMode==='off'?'Parts move only when driven by motors, contact or applied forces.':'Uses the direction and strength entered under Custom values.'}</small>
    </label>
    <details className="physics-custom" open={custom} onToggle={event=>setCustom(event.currentTarget.open)}>
      <summary>Custom values</summary>
      <p>Exact inputs for the choices above. Material sets density; grip sets friction; bounce sets restitution.</p>
      <label>Density (kg/m³)<input type="number" min={0.000001} value={density} onChange={event=>onDensity(Number(event.target.value))}/></label>
      <label>Friction coefficient<input type="number" min={0} max={10} step={0.1} value={friction} onChange={event=>onFriction(Number(event.target.value))}/></label>
      <label>Restitution (0–1)<input type="number" min={0} max={1} step={0.1} value={bounce} onChange={event=>onBounce(Number(event.target.value))}/></label>
      {(['X','Y','Z'] as const).map((axis,index)=><label key={axis}>Gravity {axis} (m/s²)<input type="number" value={gravity[index]} onChange={event=>onGravity(gravity.map((value,i)=>i===index?Number(event.target.value):value) as Vector)}/></label>)}
    </details>
  </div>;
}
