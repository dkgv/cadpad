import React,{useEffect,useRef} from 'react';
import {cameraToCssMatrix} from './view-cube-math';

export type ViewCubeDirection='iso'|'front'|'rear'|'top'|'bottom'|'left'|'right'|[number,number,number];
export {cameraToCssMatrix} from './view-cube-math';

type FaceDirection=Exclude<ViewCubeDirection,unknown[]|'iso'>;
const faces:[FaceDirection,string][]=[
  ['front','Front'],
  ['rear','Back'],
  ['right','Right'],
  ['left','Left'],
  ['top','Top'],
  ['bottom','Bottom']
];

// Screen-right and screen-down axes on each face, expressed in Z-up world space.
const axes:Record<FaceDirection,[[number,number,number],[number,number,number],[number,number,number]]>={
  front:[[0,-1,0],[1,0,0],[0,0,-1]],rear:[[0,1,0],[-1,0,0],[0,0,-1]],
  right:[[1,0,0],[0,1,0],[0,0,-1]],left:[[-1,0,0],[0,-1,0],[0,0,-1]],
  top:[[0,0,1],[1,0,0],[0,-1,0]],bottom:[[0,0,-1],[1,0,0],[0,1,0]],
};
export function cubeTarget(face:FaceDirection,column:number,row:number):[number,number,number]{
  const [normal,right,down]=axes[face];
  return normal.map((value,index)=>value+column*right[index]+row*down[index]) as [number,number,number];
}
function targetLabel([x,y,z]:[number,number,number]){
  return [z>0?'Top':z<0?'Bottom':'',y<0?'Front':y>0?'Back':'',x>0?'Right':x<0?'Left':''].filter(Boolean).join(' ');
}

type Props={
  getQuaternion:()=>[number,number,number,number]|null;
  onView:(direction:ViewCubeDirection)=>void;
};

export function ViewCube({getQuaternion,onView}:Props){
  const orient=useRef<HTMLDivElement>(null);
  const readQuaternion=useRef(getQuaternion);
  readQuaternion.current=getQuaternion;

  useEffect(()=>{
    let frame=0;
    const tick=()=>{
      const quaternion=readQuaternion.current();
      if(quaternion&&orient.current)orient.current.style.transform=cameraToCssMatrix(...quaternion);
      frame=requestAnimationFrame(tick);
    };
    frame=requestAnimationFrame(tick);
    return()=>cancelAnimationFrame(frame);
  },[]);

  return <div className="view-cube" role="group" aria-label="View orientation" onPointerDown={event=>event.stopPropagation()}>
    <div className="view-cube-stage">
      <div className="view-cube-orient" ref={orient}>
        <div className="view-cube-geom">
          {faces.map(([direction,label])=>(
            <div key={direction} className={`view-cube-face view-cube-face-${direction}`}>
              {[-1,0,1].flatMap(row=>[-1,0,1].map(column=>{
                const center=row===0&&column===0,target=cubeTarget(direction,column,row);
                const title=`${center?label:targetLabel(target)} view`;
                return <button key={`${row}:${column}`} type="button" className={`view-cube-target ${center?'face':row&&column?'corner':'edge'}`} style={row&&column?{transform:`translate(${column*50}%, ${row*50}%)`,borderRadius: '50%',zIndex:2}:undefined} aria-label={title} title={title} onClick={()=>onView(center?direction:target)}>{center?label:null}</button>;
              }))}
            </div>
          ))}
        </div>
      </div>
    </div>
  </div>;
}
