import * as THREE from 'three';
export type Vec3=[number,number,number];
export type Quaternion=[number,number,number,number];
export interface Pose {position:Vec3;quaternion:Quaternion}
export interface JointState {id:string;parent:string|null;child:string;kind:'fixed'|'prismatic'|'revolute'|'path';coordinate:number;unit:string;limits:[number,number]|null;axis_world:Vec3;origin_mm:Vec3;drivable?:boolean;path?:{length_m:number;closed:boolean;frame_rule:string;samples:PathSample[]}}
export interface CollisionPolicy {accuracy:string;clearance_m:number;backoff_m:number;coverage_method:string;ignored_pairs:[string,string][]}
export interface SimulationFrame {tick:number;time_s:number;poses:Record<string,Pose>}
export interface MotionSnapshot {simulation_unavailable_reason?:string|null;workspace_id:string;revision:string;pose_version:number;mode:'manipulate'|'simulate';recording?:{name:string;frame_count:number;duration_s:number;capacity:number;capacity_reached:boolean};playback?:{recording_id:string;name:string;frame_index:number;frame_count:number;time_s:number;duration_s:number;source_mode:string};simulation?:{external_loads?:Record<string,{force_n:Vec3;torque_nm:Vec3}>;timestep_s:number;accuracy:string;mass_kg:Record<string,number>;previous:SimulationFrame;current:SimulationFrame};cad_units:'mm';collision_checked:boolean;constraints?:{position_error_m:number;axis_error_rad:number;max_normalized_residual:number;length_tolerance_m:number;angle_tolerance_rad:number;path_certified:boolean;continuation_steps:number};collision?:CollisionPolicy|null;actor?:string|null;poses:Record<string,Pose>;joints:JointState[]}
/** Drag a rigid attachment through its nearest movable parent joint. */
export function manipulationJoint(snapshot:MotionSnapshot,instance:string):JointState|null{
  const visited=new Set<string>();let current:string|null=instance;
  while(current&&!visited.has(current)){
    visited.add(current);
    const joint=snapshot.joints.find(item=>item.child===current);
    if(!joint)return null;
    if(joint.kind!=='fixed')return joint.drivable===false?null:joint;
    current=joint.parent;
  }
  return null;
}
export function partMotion(snapshot:MotionSnapshot,instance:string):{kind:'draggable'|'linked'|'fixed';joint:JointState|null}{
  const joint=manipulationJoint(snapshot,instance);
  if(joint)return {kind:'draggable',joint};
  const visited=new Set<string>();let current:string|null=instance;
  while(current&&!visited.has(current)){
    visited.add(current);const connection=snapshot.joints.find(item=>item.child===current);
    if(!connection)break;
    if(connection.kind!=='fixed')return {kind:'linked',joint:connection};
    current=connection.parent;
  }
  return {kind:'fixed',joint:null};
}
export interface PathSample {coordinate_m:number;position_mm:Vec3}
export interface Ray {origin:Vec3;direction:Vec3}
const vector=(value:Vec3)=>new THREE.Vector3(...value);
const clamp=(joint:JointState,coordinate:number)=>joint.limits?Math.max(joint.limits[0],Math.min(joint.limits[1],coordinate)):coordinate;
export function previewJoint(snapshot:MotionSnapshot,id:string,target:number):{poses:Record<string,Pose>;coordinate:number}{
  const joint=snapshot.joints.find(joint=>joint.id===id);
  if(!joint||joint.kind==='fixed'||joint.drivable===false||!Number.isFinite(target))throw new Error('Invalid manipulation joint');
  const coordinate=clamp(joint,target),delta=coordinate-joint.coordinate;
  // Curved poses come from the authoritative analytic solver. Samples map input
  // intent only; they never substitute for exact path transforms.
  if(joint.kind==='path'||snapshot.constraints||snapshot.mode==='simulate'||snapshot.playback)return {poses:snapshot.poses,coordinate};
  const moving=new Set([joint.child]);let changed=true;
  while(changed){changed=false;for(const child of snapshot.joints)if(child.parent&&moving.has(child.parent)&&!moving.has(child.child)){moving.add(child.child);changed=true;}}
  const axis=vector(joint.axis_world),origin=vector(joint.origin_mm);
  const rotation=new THREE.Quaternion().setFromAxisAngle(axis,delta);
  const poses:Record<string,Pose>={};
  for(const [id,pose] of Object.entries(snapshot.poses)){
    if(!moving.has(id)){poses[id]=pose;continue;}
    const position=vector(pose.position),quaternion=new THREE.Quaternion(...pose.quaternion);
    if(joint.kind==='prismatic')position.addScaledVector(axis,delta*1000);
    else{position.sub(origin).applyQuaternion(rotation).add(origin);quaternion.premultiply(rotation).normalize();}
    poses[id]={position:position.toArray() as Vec3,quaternion:quaternion.toArray() as Quaternion};
  }
  return {poses,coordinate};
}
function lineCoordinate(ray:Ray,origin:Vec3,axis:Vec3):number|null{
  const a=vector(axis),d=vector(ray.direction),w=vector(origin).sub(vector(ray.origin)),cosine=a.dot(d),denominator=1-cosine*cosine;
  if(denominator<0.01)return null;
  return (-a.dot(w)+cosine*d.dot(w))/denominator;
}
function planeVector(ray:Ray,origin:Vec3,axis:Vec3):THREE.Vector3|null{
  const direction=vector(ray.direction),normal=vector(axis),denominator=direction.dot(normal);
  if(Math.abs(denominator)<0.1)return null;
  const distance=vector(origin).sub(vector(ray.origin)).dot(normal)/denominator;
  if(distance<0)return null;
  return vector(ray.origin).addScaledVector(direction,distance).sub(vector(origin));
}
/** Lock screen vs ray mapping for the whole gesture so grazing views do not flip mid-drag. */
export function sliderScreenMap(ray:Ray,origin:Vec3,axis:Vec3):boolean{return lineCoordinate(ray,origin,axis)===null;}
export function hingeScreenMap(ray:Ray,origin:Vec3,axis:Vec3):boolean{
  const offset=planeVector(ray,origin,axis);return !offset||offset.lengthSq()<1e-12;
}
/**
 * End-on sliders use stable horizontal screen distance instead of dividing by a near-zero ray/axis angle.
 * `mode: 'ray'` returns null when the current ray is unusable so the caller can keep the last target.
 */
export function sliderDelta(start:Ray,current:Ray,origin:Vec3,axis:Vec3,screenDX:number,mmPerPixel:number,mode:'auto'|'ray'|'screen'='auto'):number|null{
  if(mode==='screen')return screenDX*mmPerPixel/1000;
  const a=lineCoordinate(start,origin,axis),b=lineCoordinate(current,origin,axis);
  if(a!==null&&b!==null)return (b-a)/1000;
  return mode==='ray'?null:screenDX*mmPerPixel/1000;
}
/**
 * Edge-on hinges use horizontal screen angle scaled by view mm/pixel against a 100 mm reference radius.
 * A pointer over the pivot has no angular target (`angle: null`); keep the previous angle.
 */
export function hingeDelta(start:Ray,current:Ray,origin:Vec3,axis:Vec3,screenDX:number,mmPerPixel=1,mode:'auto'|'ray'|'screen'='auto'):{angle:number|null;fallback:boolean}{
  const screen=()=>({angle:screenDX*mmPerPixel/100,fallback:true as const});
  if(mode==='screen')return screen();
  const a=planeVector(start,origin,axis),b=planeVector(current,origin,axis);
  if(mode==='auto'&&(!a||a.lengthSq()<1e-12))return screen();
  if(!a||a.lengthSq()<1e-12||!b||b.lengthSq()<1e-12)return {angle:null,fallback:false};
  a.normalize();b.normalize();return {angle:Math.atan2(vector(axis).dot(a.clone().cross(b)),a.dot(b)),fallback:false};
}
export function unwrapStep(previous:number,current:number):number{return Math.atan2(Math.sin(current-previous),Math.cos(current-previous));}

/** Approximate pointer intent on a sampled path; continuity resolves crossing/seam ties. */
export function pathCoordinate(ray:Ray,samples:PathSample[],previous:number,screenDX:number,mmPerPixel:number):number{
  const query=new THREE.Ray(vector(ray.origin),vector(ray.direction));let best:{distance:number;coordinate:number;aligned:boolean}|null=null;
  const tie=Math.max(1e-9,mmPerPixel*2)**2;
  for(let i=0;i+1<samples.length;i++){
    const a=vector(samples[i].position_mm),b=vector(samples[i+1].position_mm),direction=b.clone().sub(a),length=direction.lengthSq();if(length<1e-20)continue;
    const onRay=new THREE.Vector3(),onSegment=new THREE.Vector3();const distance=query.distanceSqToSegment(a,b,onRay,onSegment);
    const fraction=Math.max(0,Math.min(1,onSegment.clone().sub(a).dot(direction)/length));
    const coordinate=samples[i].coordinate_m+(samples[i+1].coordinate_m-samples[i].coordinate_m)*fraction;
    const aligned=Math.abs(direction.normalize().dot(query.direction))>0.995;
    if(!best||distance<best.distance-tie||Math.abs(distance-best.distance)<=tie&&Math.abs(coordinate-previous)<Math.abs(best.coordinate-previous))best={distance,coordinate,aligned};
  }
  if(!best)return previous;
  const coordinate=best.aligned?previous+screenDX*mmPerPixel/1000:best.coordinate;
  return Math.max(samples[0].coordinate_m,Math.min(samples.at(-1)!.coordinate_m,coordinate));
}

/** Display-only interpolation of adjacent authoritative ticks. Exact queries use
 * snapshot.poses and its pose_version, never these intermediate render poses. */
export function interpolateSimulation(snapshot:MotionSnapshot,alpha:number):Record<string,Pose>{
  const simulation=snapshot.simulation;
  if(snapshot.mode!=='simulate'||!simulation||simulation.current.tick!==simulation.previous.tick+1)return snapshot.poses;
  const fraction=Number.isFinite(alpha)?Math.max(0,Math.min(1,alpha)):1;
  const poses:Record<string,Pose>={};
  for(const [id,current] of Object.entries(simulation.current.poses)){
    const previous=simulation.previous.poses[id];if(!previous){poses[id]=current;continue;}
    poses[id]={position:vector(previous.position).lerp(vector(current.position),fraction).toArray() as Vec3,quaternion:new THREE.Quaternion(...previous.quaternion).slerp(new THREE.Quaternion(...current.quaternion),fraction).normalize().toArray() as Quaternion};
  }
  return poses;
}

/** Playback selects an existing sample; it never invents a new measured pose. */
export function recordedFrameAt(times:number[],time:number):number {
  if(!times.length)return 0;
  const target=Number.isFinite(time)?Math.max(0,time):0;
  let low=0,high=times.length;
  while(low<high){const middle=(low+high)>>>1;if(times[middle]<=target)low=middle+1;else high=middle;}
  return Math.max(0,low-1);
}
