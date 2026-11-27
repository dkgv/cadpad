import * as THREE from 'three';
import {MeshBVH} from 'three-mesh-bvh';

type Vec3=[number,number,number];
/** Display surface of one part at its rest placement. Arrays are the revision's mesh buffers. */
export interface ReviewPart {instance_id:string;name:string;position:Vec3;volume:number|null;vertices:Float32Array;triangles:Uint32Array}
export interface ReviewSnapshot {workspace:string;revision:string;parts:ReviewPart[]}
export interface ReviewDiff {
  /** Non-indexed world-space triangles present only after the change. */
  added:Float32Array;
  /** Non-indexed world-space triangles present only before the change. */
  removed:Float32Array;
  addedArea:number;removedArea:number;
  volumeBefore:number|null;volumeAfter:number|null;
  /** Surfaces closer than this (model units) count as unchanged. */
  tolerance:number;
  identical:boolean;
}
export interface DiffOptions {
  /** Upper bound on surface samples across both sides; coarser sampling keeps large models responsive. */
  budget?:number;
  /** Relative to the combined bounding-box diagonal. */
  tolerance?:number;
}

function sameArray(a:ArrayLike<number>,b:ArrayLike<number>){
  if(a===b)return true;
  if(a.length!==b.length)return false;
  for(let i=0;i<a.length;i++)if(a[i]!==b[i])return false;
  return true;
}
function samePart(a:ReviewPart,b:ReviewPart){
  return sameArray(a.position,b.position)&&sameArray(a.triangles,b.triangles)&&sameArray(a.vertices,b.vertices);
}
/** World-space triangle soup for parts not shared verbatim by the other revision. */
function soup(parts:ReviewPart[]){
  let count=0;
  for(const part of parts)count+=Math.floor(part.triangles.length/3)*9;
  const out=new Float32Array(count);let o=0;
  for(const {vertices,triangles,position:[x,y,z]} of parts){
    const limit=vertices.length/3;
    for(let t=0;t+2<triangles.length;t+=3)for(let k=0;k<3;k++){
      const v=triangles[t+k];
      if(v>=limit)throw new Error('Mesh triangle index is out of range');
      out[o++]=vertices[v*3]+x;out[o++]=vertices[v*3+1]+y;out[o++]=vertices[v*3+2]+z;
    }
  }
  return out;
}
function triangleArea(p:Float32Array|number[],i:number){
  const ax=p[i+3]-p[i],ay=p[i+4]-p[i+1],az=p[i+5]-p[i+2],bx=p[i+6]-p[i],by=p[i+7]-p[i+1],bz=p[i+8]-p[i+2];
  const cx=ay*bz-az*by,cy=az*bx-ax*bz,cz=ax*by-ay*bx;
  return Math.sqrt(cx*cx+cy*cy+cz*cz)/2;
}
function surfaceArea(points:Float32Array){let total=0;for(let i=0;i+8<points.length;i+=9)total+=triangleArea(points,i);return total;}
function bvh(points:Float32Array){
  if(!points.length)return null;
  const geometry=new THREE.BufferGeometry();
  geometry.setAttribute('position',new THREE.BufferAttribute(points,3));
  return new MeshBVH(geometry);
}
function totalVolume(parts:ReviewPart[]){
  let total=0;
  for(const part of parts){if(part.volume==null)return null;total+=part.volume;}
  return total;
}

/**
 * Keep the pieces of `source` that lie farther than `tolerance` from `target`.
 * Triangles are bisected along their longest edge until no edge exceeds `step`,
 * so a large face that only changed in its middle reports just that region.
 */
function exclusive(source:Float32Array,target:MeshBVH|null,step:number,tolerance:number){
  if(!target)return {points:source,area:surfaceArea(source)};
  const out:number[]=[];let area=0;
  const probe=new THREE.Vector3(),hit={point:new THREE.Vector3(),distance:0,faceIndex:0};
  const step2=step*step;
  const stack:number[]=[];
  for(let i=0;i+8<source.length;i+=9){
    stack.length=0;
    for(let k=0;k<9;k++)stack.push(source[i+k]);
    while(stack.length){
      const t=stack.splice(stack.length-9,9);
      const d01=(t[0]-t[3])**2+(t[1]-t[4])**2+(t[2]-t[5])**2;
      const d12=(t[3]-t[6])**2+(t[4]-t[7])**2+(t[5]-t[8])**2;
      const d20=(t[6]-t[0])**2+(t[7]-t[1])**2+(t[8]-t[2])**2;
      const longest=Math.max(d01,d12,d20);
      if(longest>step2){
        // Split edge (a,b) at its midpoint m: triangles (a,m,c) and (m,b,c).
        const [a,b,c]=longest===d01?[0,1,2]:longest===d12?[1,2,0]:[2,0,1];
        const m=[0,1,2].map(axis=>(t[a*3+axis]+t[b*3+axis])/2);
        const at=(v:number)=>t.slice(v*3,v*3+3);
        stack.push(...at(a),...m,...at(c),...m,...at(b),...at(c));
        continue;
      }
      probe.set((t[0]+t[3]+t[6])/3,(t[1]+t[4]+t[7])/3,(t[2]+t[5]+t[8])/3);
      // maxThreshold only prunes bounds; a leaf can still return a farther triangle.
      const near=target.closestPointToPoint(probe,hit,tolerance,tolerance);
      if(near&&near.distance<=tolerance)continue;
      out.push(...t);area+=triangleArea(t,0);
    }
  }
  return {points:Float32Array.from(out),area};
}

/** Compare two revisions' display surfaces. Parts shared verbatim are skipped. */
export function diffSurfaces(before:ReviewPart[],after:ReviewPart[],options:DiffOptions={}):ReviewDiff{
  const volumeBefore=totalVolume(before),volumeAfter=totalVolume(after);
  const unchanged=new Set<ReviewPart>();
  const afterById=new Map(after.map(part=>[part.instance_id,part]));
  for(const part of before){
    const match=afterById.get(part.instance_id);
    if(match&&samePart(part,match)){unchanged.add(part);unchanged.add(match);}
  }
  const changedBefore=before.filter(part=>!unchanged.has(part)),changedAfter=after.filter(part=>!unchanged.has(part));
  const empty={added:new Float32Array(),removed:new Float32Array(),addedArea:0,removedArea:0,volumeBefore,volumeAfter};
  if(!changedBefore.length&&!changedAfter.length)return {...empty,tolerance:0,identical:true};
  // Surfaces of verbatim parts still occlude: removed material that a shared part now covers is not removed.
  const sharedAfter=after.filter(part=>unchanged.has(part));
  const beforeSoup=soup(changedBefore),afterSoup=soup(changedAfter),sharedSoup=soup(sharedAfter);
  const box=new THREE.Box3();
  for(const points of [beforeSoup,afterSoup])for(let i=0;i+2<points.length;i+=3)box.expandByPoint(probeAt(points,i));
  const diagonal=box.isEmpty()?0:box.min.distanceTo(box.max);
  if(diagonal===0)return {...empty,tolerance:0,identical:false};
  const tolerance=Math.max(diagonal*(options.tolerance??0.003),1e-6);
  const budget=options.budget??250_000;
  const area=surfaceArea(beforeSoup)+surfaceArea(afterSoup);
  const step=Math.max(diagonal/120,Math.sqrt(4*area/budget));
  const afterAll=sharedSoup.length?concat(afterSoup,sharedSoup):afterSoup;
  const removed=exclusive(beforeSoup,bvh(afterAll),step,tolerance);
  const added=exclusive(afterSoup,bvh(beforeSoup.length?concat(beforeSoup,sharedSoup):sharedSoup),step,tolerance);
  return {added:added.points,removed:removed.points,addedArea:added.area,removedArea:removed.area,volumeBefore,volumeAfter,tolerance,identical:!added.points.length&&!removed.points.length};
}
function probeAt(points:Float32Array,i:number){return new THREE.Vector3(points[i],points[i+1],points[i+2]);}
function concat(a:Float32Array,b:Float32Array){const out=new Float32Array(a.length+b.length);out.set(a);out.set(b,a.length);return out;}
