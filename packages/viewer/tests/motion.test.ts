import test from 'node:test';
import assert from 'node:assert/strict';
import {previewJoint,sliderDelta,hingeDelta,unwrapStep,sliderScreenMap,hingeScreenMap,type MotionSnapshot} from '../src/motion.ts';
const near=(actual:number[],expected:number[])=>{assert.equal(actual.length,expected.length);actual.forEach((v,i)=>assert.ok(Math.abs(v-expected[i])<1e-9,`${actual} != ${expected}`));};
const scene:MotionSnapshot={workspace_id:'w',revision:'r',pose_version:0,mode:'manipulate',cad_units:'mm',collision_checked:false,poses:{rail:{position:[0,0,0],quaternion:[0,0,0,1]},carriage:{position:[20,0,0],quaternion:[0,0,0,1]},arm:{position:[40,0,0],quaternion:[0,0,0,1]}},joints:[{id:'slide',parent:'rail',child:'carriage',kind:'prismatic',coordinate:0.02,unit:'m',limits:[0,0.08],axis_world:[1,0,0],origin_mm:[0,0,0]},{id:'hinge',parent:'carriage',child:'arm',kind:'revolute',coordinate:0,unit:'rad',limits:[-Math.PI,Math.PI],axis_world:[0,0,1],origin_mm:[30,0,0]}]};
test('rigid preview clamps coordinates and moves precisely the driven subtree',()=>{
 const slider=previewJoint(scene,'slide',1);assert.equal(slider.coordinate,0.08);near(slider.poses.carriage.position,[80,0,0]);near(slider.poses.arm.position,[100,0,0]);near(slider.poses.rail.position,[0,0,0]);near(scene.poses.carriage.position,[20,0,0]);
 const hinge=previewJoint(scene,'hinge',Math.PI/2);near(hinge.poses.arm.position,[30,10,0]);near(hinge.poses.carriage.position,[20,0,0]);near(hinge.poses.arm.quaternion,[0,0,Math.SQRT1_2,Math.SQRT1_2]);assert.throws(()=>previewJoint(scene,'missing',1));assert.throws(()=>previewJoint(scene,'slide',NaN));
});
test('slider rays use millimetre geometry and switch to bounded screen mapping end-on',()=>{
 const start={origin:[0,0,100] as [number,number,number],direction:[0,0,-1] as [number,number,number]};
 const moved={...start,origin:[30,0,100] as [number,number,number]};
 assert.equal(sliderDelta(start,moved,[0,0,0],[1,0,0],100,2),0.03);
 assert.equal(sliderDelta(start,moved,[0,0,0],[0,0,1],100,2),0.2);
 assert.equal(sliderScreenMap(start,[0,0,0],[1,0,0]),false);
 assert.equal(sliderScreenMap(start,[0,0,0],[0,0,1]),true);
 assert.equal(sliderDelta(start,moved,[0,0,0],[1,0,0],100,2,'screen'),0.2);
 assert.equal(sliderDelta(start,{...start,origin:[0,0,100]},[0,0,0],[1,0,0],0,2,'ray'),0);
 assert.equal(sliderDelta(start,{...start,direction:[0,0.01,-1]},[0,0,0],[0,0,1],50,2,'ray'),null);
});
test('hinge rays preserve signed rotation, branch continuity, and stable edge-on mapping',()=>{
 const start={origin:[10,0,100] as [number,number,number],direction:[0,0,-1] as [number,number,number]};
 const current={...start,origin:[0,10,100] as [number,number,number]};
 const delta=hingeDelta(start,current,[0,0,0],[0,0,1],100);assert.equal(delta.fallback,false);assert.equal(delta.angle,Math.PI/2);
 assert.equal(hingeScreenMap(start,[0,0,0],[0,0,1]),false);
 assert.equal(hingeScreenMap(start,[0,0,0],[1,0,0]),true);
 const edge=hingeDelta(start,current,[0,0,0],[1,0,0],100,1);assert.equal(edge.fallback,true);assert.equal(edge.angle,1);
 const locked=hingeDelta(start,current,[0,0,0],[1,0,0],100,2,'screen');assert.equal(locked.fallback,true);assert.equal(locked.angle,2);
 const pivot=hingeDelta(start,{...start,origin:[0,0,100]},[0,0,0],[0,0,1],0,1,'ray');assert.equal(pivot.angle,null);
 assert.ok(Math.abs(unwrapStep(3.1,-3.1)-(2*Math.PI-6.2))<1e-12);
});

test('sampled path input follows ray intent and keeps the nearby branch at a closed seam',async()=>{
 const {pathCoordinate}=await import('../src/motion.ts');
 const samples=[{coordinate_m:0,position_mm:[0,0,0] as [number,number,number]},{coordinate_m:0.1,position_mm:[100,0,0] as [number,number,number]},{coordinate_m:0.2,position_mm:[100,100,0] as [number,number,number]},{coordinate_m:0.3,position_mm:[0,100,0] as [number,number,number]},{coordinate_m:0.4,position_mm:[0,0,0] as [number,number,number]}];
 const ray={origin:[50,0,100] as [number,number,number],direction:[0,0,-1] as [number,number,number]};
 assert.ok(Math.abs(pathCoordinate(ray,samples,0.04,0,0.1)-0.05)<1e-12);
 const seam={...ray,origin:[0,0,100] as [number,number,number]};assert.equal(pathCoordinate(seam,samples,0.39,0,0.1),0.4);assert.equal(pathCoordinate(seam,samples,0.01,0,0.1),0);
});
test('end-on paths have a stable screen fallback and samples do not create speculative poses',async()=>{
 const {pathCoordinate}=await import('../src/motion.ts');const samples=[{coordinate_m:0,position_mm:[0,0,0] as [number,number,number]},{coordinate_m:0.1,position_mm:[0,0,100] as [number,number,number]}];const ray={origin:[0,0,200] as [number,number,number],direction:[0,0,-1] as [number,number,number]};assert.ok(Math.abs(pathCoordinate(ray,samples,0.02,10,2)-0.04)<1e-12);
 const curved:MotionSnapshot={...scene,joints:[{...scene.joints[0],kind:'path',path:{length_m:0.1,closed:false,frame_rule:'rotation_minimizing_with_closed_seam_roll',samples}}]};const preview=previewJoint(curved,'slide',0.08);assert.equal(preview.coordinate,0.08);assert.equal(preview.poses,curved.poses);
});

test('coupled mechanisms render authoritative poses and reject passive preview targets',()=>{
 const coupled:MotionSnapshot={...scene,constraints:{position_error_m:0,axis_error_rad:0,max_normalized_residual:0,length_tolerance_m:1e-6,angle_tolerance_rad:1e-5,path_certified:true,continuation_steps:8},joints:[{...scene.joints[0],drivable:true},{...scene.joints[1],drivable:false}]};
 const preview=previewJoint(coupled,'slide',0.06);assert.equal(preview.poses,coupled.poses);assert.equal(preview.coordinate,0.06);assert.throws(()=>previewJoint(coupled,'hinge',0.1));
});

test('simulation previews do not teleport and render interpolation uses only adjacent authoritative ticks',async()=>{
 const {interpolateSimulation}=await import('../src/motion.ts');
 const previous={tick:4,time_s:4/120,poses:scene.poses};
 const current={tick:5,time_s:5/120,poses:{...scene.poses,carriage:{position:[40,0,0] as [number,number,number],quaternion:[0,0,1,0] as [number,number,number,number]}}};
 const simulated:MotionSnapshot={...scene,mode:'simulate',poses:current.poses,simulation:{timestep_s:1/120,accuracy:'approximate',mass_kg:{carriage:1},previous,current}};
 assert.equal(previewJoint(simulated,'slide',0.07).poses,simulated.poses);
 const half=interpolateSimulation(simulated,0.5);near(half.carriage.position,[30,0,0]);near(half.carriage.quaternion,[0,0,Math.SQRT1_2,Math.SQRT1_2]);
 near(interpolateSimulation(simulated,-1).carriage.position,[20,0,0]);near(interpolateSimulation(simulated,2).carriage.position,[40,0,0]);
 const skipped={...simulated,simulation:{...simulated.simulation!,current:{...current,tick:8}}};assert.equal(interpolateSimulation(skipped,0.5),skipped.poses);
 assert.equal(simulated.poses.carriage.position[0],40);
});

test('recorded playback chooses accepted samples at boundaries, pauses and long gaps',async()=>{
 const {recordedFrameAt}=await import('../src/motion.ts');
 const times=[0,1/120,2/120,0.5,4];
 assert.equal(recordedFrameAt(times,-1),0);assert.equal(recordedFrameAt(times,0),0);
 assert.equal(recordedFrameAt(times,1/120),1);assert.equal(recordedFrameAt(times,0.2),2);
 assert.equal(recordedFrameAt(times,0.5),3);assert.equal(recordedFrameAt(times,100),4);
 assert.equal(recordedFrameAt(times,NaN),0);assert.equal(recordedFrameAt([],2),0);
 const playback:MotionSnapshot={...scene,playback:{recording_id:'record',name:'Recorded pose',frame_index:1,frame_count:2,time_s:0.1,duration_s:0.1,source_mode:'manipulate'}};
 assert.equal(previewJoint(playback,'slide',0.08).poses,scene.poses);
});

test('canvas drag resolves rigid attachments and rejects fixed roots and passive joints',async()=>{
 const {manipulationJoint}=await import('../src/motion.ts');
 const assembly:MotionSnapshot={...scene,joints:[...scene.joints,{...scene.joints[0],id:'fixture',parent:'carriage',child:'handle',kind:'fixed'}]};
 assert.equal(manipulationJoint(assembly,'handle')?.id,'slide');
 assert.equal(manipulationJoint(assembly,'arm')?.id,'hinge');
 assert.equal(manipulationJoint(assembly,'rail'),null);
 assert.equal(manipulationJoint({...assembly,joints:assembly.joints.map(j=>j.id==='slide'?{...j,drivable:false}:j)},'handle'),null);
 assert.equal(manipulationJoint({...assembly,joints:[{...scene.joints[0],kind:'fixed',parent:'carriage'}]},'carriage'),null);
});

test('motion capability distinguishes direct dragging, rigid attachments, followers and fixed bases',async()=>{
 const {partMotion}=await import('../src/motion.ts');
 const assembly:MotionSnapshot={...scene,joints:[...scene.joints,{...scene.joints[0],id:'attachment',parent:'carriage',child:'handle',kind:'fixed'},{...scene.joints[1],id:'follower-joint',child:'follower',drivable:false}]};
 assert.equal(partMotion(assembly,'carriage').kind,'draggable');
 assert.equal(partMotion(assembly,'handle').joint?.id,'slide');
 assert.equal(partMotion(assembly,'follower').kind,'linked');
 assert.equal(partMotion(assembly,'rail').kind,'fixed');
 assert.equal(partMotion(assembly,'unknown').kind,'fixed');
});
