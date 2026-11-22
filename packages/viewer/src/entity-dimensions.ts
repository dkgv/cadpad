import * as THREE from 'three';
import type {PickGeometry} from './picking';

type Vec3 = [number,number,number];
export interface EntityDimension {
  id:string;kind:'overall'|'distance'|'diameter';part:string;start:Vec3;end:Vec3;value:number;label:string;
  definition?:{kind:'overall';instance_id:string;axis:'x'|'y'|'z'};
}

const AXES=['x','y','z'] as const;
const AXIS_LABELS=['Width','Depth','Height'];
const EPS=1e-6;
const v3=(v:THREE.Vector3)=>v.toArray() as Vec3;

/** Part-local dimensions of one selected face or edge. Coordinates share the
 * part frame of the pick geometry, so placement follows the part transform. */
export function entityDimensions(instance:string,entityId:string,geometry:PickGeometry):EntityDimension[]{
  const [kind,indexText]=entityId.split(':');
  const id=`${entityId}@${instance}`;
  if(kind==='face'){
    const range=geometry.faces.range(geometry.faceIds.indexOf(`face:${indexText}`));
    const {vertices,triangles}=geometry.buffers;
    if(!range||!vertices||!triangles)return [];
    const points:THREE.Vector3[]=[],seen=new Set<number>();
    for(let i=range[0]*3;i<range[1]*3;i++){const v=triangles[i];if(!seen.has(v)){seen.add(v);points.push(new THREE.Vector3(vertices[v*3],vertices[v*3+1],vertices[v*3+2]));}}
    return faceDimensions(id,instance,points);
  }
  if(kind==='edge'){
    const range=geometry.edges.range(geometry.edgeIds.indexOf(`edge:${indexText}`));
    const edges=geometry.buffers.edges;
    if(!range||!edges||range[0]===range[1])return [];
    const segments:[THREE.Vector3,THREE.Vector3][]=[];
    for(let s=range[0];s<range[1];s++){const o=s*6;segments.push([new THREE.Vector3(edges[o],edges[o+1],edges[o+2]),new THREE.Vector3(edges[o+3],edges[o+4],edges[o+5])]);}
    return edgeDimensions(id,instance,segments);
  }
  return [];
}

function axisBox(id:string,instance:string,points:THREE.Vector3[],prefix:string):EntityDimension[]{
  const box=new THREE.Box3().setFromPoints(points);
  if(box.isEmpty())return [];
  const size=box.getSize(new THREE.Vector3());
  return AXES.flatMap((axis,index):EntityDimension[]=>{
    if(size.getComponent(index)<EPS)return [];
    const start=box.min.clone(),end=start.clone();end.setComponent(index,box.max.getComponent(index));
    return [{id:`${id}:${axis}`,kind:'overall',part:instance,start:v3(start),end:v3(end),value:size.getComponent(index),label:`${prefix} ${AXIS_LABELS[index].toLowerCase()}`,definition:{kind:'overall',instance_id:instance,axis}}];
  });
}

function circle(points:THREE.Vector3[]){
  if(points.length<8)return null;
  const center=points.reduce((sum,p)=>sum.add(p),new THREE.Vector3()).divideScalar(points.length);
  const radii=points.map(p=>p.distanceTo(center)),radius=radii.reduce((a,b)=>a+b,0)/radii.length;
  if(radius<EPS||radii.some(r=>Math.abs(r-radius)>radius*0.01))return null;
  // Points must also be coplanar to be a circle rather than a sphere sampling.
  const u=points[0].clone().sub(center).normalize();
  const far=points.reduce((best,p)=>{const d=p.clone().sub(center);const c=d.clone().cross(u).length();return c>best.c?{c,d}:best;},{c:0,d:new THREE.Vector3()});
  if(far.c<radius*0.1)return null;
  const normal=u.clone().cross(far.d).normalize();
  if(points.some(p=>Math.abs(p.clone().sub(center).dot(normal))>radius*0.01))return null;
  return {center,radius,u};
}

function diameter(id:string,instance:string,c:{center:THREE.Vector3;radius:number;u:THREE.Vector3},label:string):EntityDimension{
  const offset=c.u.clone().multiplyScalar(c.radius);
  return {id:`${id}:diameter`,kind:'diameter',part:instance,start:v3(c.center.clone().sub(offset)),end:v3(c.center.clone().add(offset)),value:c.radius*2,label};
}

function faceDimensions(id:string,instance:string,points:THREE.Vector3[]):EntityDimension[]{
  if(points.length<3)return [];
  // Plane from the most spread-out triple; a robust stand-in for a fit.
  const a=points[0];
  const b=points.reduce((best,p)=>p.distanceToSquared(a)>best.distanceToSquared(a)?p:best,a);
  const ab=b.clone().sub(a);
  let normal=new THREE.Vector3();
  for(const p of points){const n=ab.clone().cross(p.clone().sub(a));if(n.lengthSq()>normal.lengthSq())normal=n;}
  const scale=Math.sqrt(ab.lengthSq());
  if(normal.lengthSq()<EPS||scale<EPS)return axisBox(id,instance,points,'Face');
  normal.normalize();
  const planar=points.every(p=>Math.abs(p.clone().sub(a).dot(normal))<=Math.max(EPS,scale*1e-4));
  if(!planar)return axisBox(id,instance,points,'Face');
  // A flat disc (no hole) reads best as one diameter.
  const centroid=points.reduce((s,q)=>s.add(q),new THREE.Vector3()).divideScalar(points.length);
  const rim=circle(points.filter(p=>p.distanceTo(centroid)>EPS));
  if(rim&&rim.radius>EPS)return [diameter(id,instance,rim,'Face')];
  // Axis-aligned planes use the part axes and extension lines.
  const dominant=[0,1,2].find(i=>Math.abs(normal.getComponent(i))>1-1e-6);
  if(dominant!==undefined)return axisBox(id,instance,points,'Face');
  // Tilted plane: in-plane frame anchored to the part axis closest to the plane.
  const axis=[0,1,2].map(i=>new THREE.Vector3().setComponent(i,1)).sort((p,q)=>Math.abs(p.dot(normal))-Math.abs(q.dot(normal)))[0];
  const u=axis.sub(normal.clone().multiplyScalar(axis.dot(normal))).normalize(),v=normal.clone().cross(u).normalize();
  let minU=Infinity,maxU=-Infinity,minV=Infinity,maxV=-Infinity;
  for(const p of points){const d=p.clone().sub(a),pu=d.dot(u),pv=d.dot(v);minU=Math.min(minU,pu);maxU=Math.max(maxU,pu);minV=Math.min(minV,pv);maxV=Math.max(maxV,pv);}
  const at=(pu:number,pv:number)=>a.clone().add(u.clone().multiplyScalar(pu)).add(v.clone().multiplyScalar(pv));
  const corner=at(minU,minV);
  const out:EntityDimension[]=[];
  if(maxU-minU>EPS)out.push({id:`${id}:u`,kind:'distance',part:instance,start:v3(corner),end:v3(at(maxU,minV)),value:maxU-minU,label:'Face width'});
  if(maxV-minV>EPS)out.push({id:`${id}:v`,kind:'distance',part:instance,start:v3(corner),end:v3(at(minU,maxV)),value:maxV-minV,label:'Face height'});
  return out;
}

function edgeDimensions(id:string,instance:string,segments:[THREE.Vector3,THREE.Vector3][]):EntityDimension[]{
  const first=segments[0][0],last=segments[segments.length-1][1];
  const length=segments.reduce((sum,[s,e])=>sum+s.distanceTo(e),0);
  if(length<EPS)return [];
  const chord=first.distanceTo(last);
  if(Math.abs(chord-length)<=Math.max(EPS,length*1e-4))
    return [{id:`${id}:length`,kind:'distance',part:instance,start:v3(first),end:v3(last),value:length,label:'Edge length'}];
  const points=segments.map(([s])=>s);
  const ring=chord<=Math.max(EPS,length*1e-3)?circle(points):null;
  if(ring)return [diameter(id,instance,ring,'Edge')];
  // Open curve: chord drawn, true path length reported, plus its extents.
  const out:EntityDimension[]=[];
  if(chord>EPS)out.push({id:`${id}:length`,kind:'distance',part:instance,start:v3(first),end:v3(last),value:length,label:'Edge length'});
  return [...out,...axisBox(id,instance,[...points,last],'Edge')];
}
