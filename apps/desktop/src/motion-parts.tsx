import React from 'react';
import {partMotion,type MotionSnapshot,type SceneModel} from '@cadpad/viewer';
import {JointControl} from './joint-control';
type Joint=MotionSnapshot['joints'][number];
export function MotionParts({parts,motion,dragging,disabled,coordinates,onDragging,onLocate,onPreview,onCommit}:{parts:SceneModel['parts'];motion:MotionSnapshot;dragging:boolean;disabled:boolean;coordinates:Map<string,number>;onDragging:(enabled:boolean)=>void;onLocate:(id:string)=>void;onPreview:(joint:Joint,value:number)=>void;onCommit:(joint:Joint,value:number)=>void}){
  const joints=motion.joints.filter(joint=>joint.kind!=='fixed'&&joint.drivable!==false);
  const others=parts.filter(part=>!joints.some(joint=>joint.child===part.instance_id));
  return <div className="motion-parts" aria-label="Objects and movement">
    <div className="motion-move-toolbar"><span>{disabled?'Leave playback to move objects.':dragging?'Drag a teal object in the canvas.':'Move a slider, or enable canvas dragging.'}</span><label><input type="checkbox" aria-label="Drag in canvas" checked={dragging} disabled={disabled} onChange={event=>onDragging(event.target.checked)}/>Drag in canvas</label></div>
    <div className="joint-controls">{joints.map(joint=><JointControl key={`${motion.revision}:${joint.id}`} joint={joint} name={parts.find(part=>part.instance_id===joint.child)?.name||joint.id} coordinate={coordinates.get(joint.id)??joint.coordinate} disabled={disabled} motor={false} onLocate={()=>onLocate(joint.child)} onPreview={value=>onPreview(joint,value)} onCommit={value=>onCommit(joint,value)}/>)}</div>
    {!!others.length&&<ul className="motion-other-parts" aria-label="Other objects">{others.map(part=>{
      const {kind,joint}=partMotion(motion,part.instance_id);
      const label=kind==='fixed'?'Fixed':kind==='linked'?'Follows mechanism':`Moves with ${parts.find(item=>item.instance_id===joint?.child)?.name||joint?.child}`;
      return <li key={part.instance_id}><button aria-label={`Locate ${part.name} in canvas`} onClick={()=>onLocate(part.instance_id)}>{part.name}</button><span className={`part-motion-badge ${kind}`}>{label}</span></li>;
    })}</ul>}
  </div>;
}
