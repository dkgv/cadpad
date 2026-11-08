import * as THREE from 'three';

export type SelectionMode = 'face'|'edge'|'part';
export type MeshBuffers = Record<string,Float32Array|Uint32Array>;
/** Half-open primitive ranges retain zero-length topology entries. */
export class PrimitiveSpans {
  readonly ends:readonly number[];
  constructor(counts:ArrayLike<number>,total:number){
    const ends:number[]=[];let end=0;
    for(let i=0;i<counts.length;i++){
      const count=counts[i];
      if(!Number.isSafeInteger(count)||count<0)throw new Error('Invalid topology span');
      end+=count;ends.push(end);
    }
    if(end!==total)throw new Error('Topology spans do not match display primitives');
    this.ends=Object.freeze(ends);
  }
  entity(primitive:number):number|null{
    if(!Number.isSafeInteger(primitive)||primitive<0||primitive>=(this.ends.at(-1)??0))return null;
    let low=0,high=this.ends.length;
    while(low<high){const middle=(low+high)>>>1;if(this.ends[middle]<=primitive)low=middle+1;else high=middle;}
    return low;
  }
  range(entity:number):[number,number]|null{
    return Number.isSafeInteger(entity)&&entity>=0&&entity<this.ends.length?[entity?this.ends[entity-1]:0,this.ends[entity]]:null;
  }
}
export interface PickTopology {face_ids:(string|null)[];edge_ids:(string|null)[]}
/** Immutable CPU-only topology and geometry; each occurrence owns its raycast pose. */
class PickResources {
  readonly faces:PrimitiveSpans;
  readonly edges:PrimitiveSpans;
  readonly faceIds:readonly (string|null)[];
  readonly edgeIds:readonly (string|null)[];
  readonly surface:THREE.BufferGeometry;
  readonly lines:THREE.BufferGeometry;
  users=0;
  released=false;
  readonly buffers:MeshBuffers;
  private onRelease:()=>void;
  constructor(buffers:MeshBuffers,topology?:PickTopology,onRelease:()=>void=()=>{}){
    this.onRelease=onRelease;
    this.buffers=Object.freeze({...buffers});
    const vertices=buffers.vertices??new Float32Array(),triangles=buffers.triangles??new Uint32Array(),edges=buffers.edges??new Float32Array();
    if(vertices.length%3||triangles.length%3||edges.length%6)throw new Error('Invalid display geometry');
    for(const index of triangles)if(index>=vertices.length/3)throw new Error('Display triangle index is out of bounds');
    this.faces=new PrimitiveSpans(buffers.triangles_per_face??[],triangles.length/3);
    this.edges=new PrimitiveSpans(buffers.segments_per_edge??[],edges.length/6);
    this.faceIds=Object.freeze([...(topology?.face_ids??this.faces.ends.map((_,index)=>`face:${index}`))]);
    this.edgeIds=Object.freeze([...(topology?.edge_ids??this.edges.ends.map((_,index)=>`edge:${index}`))]);
    for(const [kind,ids,spans] of [['face',this.faceIds,this.faces],['edge',this.edgeIds,this.edges]] as const){
      if(ids.length!==spans.ends.length||ids.some(id=>id!==null&&!new RegExp(`^${kind}:[0-9]+$`).test(id))||new Set(ids.filter(id=>id!==null)).size!==ids.filter(id=>id!==null).length)throw new Error('Invalid display topology identities');
    }
    this.lines=new THREE.BufferGeometry().setAttribute('position',new THREE.BufferAttribute(edges,3));
    this.surface=new THREE.BufferGeometry().setAttribute('position',new THREE.BufferAttribute(vertices,3)).setIndex(new THREE.BufferAttribute(triangles,1));
  }
  release(){if(--this.users===0){this.released=true;this.surface.dispose();this.lines.dispose();this.onRelease();}}
}
export class PickGeometry {
  readonly faces:PrimitiveSpans;
  readonly edges:PrimitiveSpans;
  readonly lines:THREE.LineSegments;
  readonly surface:THREE.Mesh;
  readonly buffers:MeshBuffers;
  readonly faceIds:readonly (string|null)[];
  readonly edgeIds:readonly (string|null)[];
  private disposed=false;
  /** The third argument is internal to the scoped pool; callers keep buffer bytes immutable. */
  private resources:PickResources;
  constructor(buffers:MeshBuffers,topology?:PickTopology,resources=new PickResources(buffers,topology)){
    this.resources=resources;
    if(resources.released)throw new Error('Picking resources were released');
    this.buffers=resources.buffers;this.faces=resources.faces;this.edges=resources.edges;
    this.faceIds=resources.faceIds;this.edgeIds=resources.edgeIds;resources.users++;
    this.lines=new THREE.LineSegments(resources.lines,new THREE.LineBasicMaterial());this.lines.matrixAutoUpdate=false;
    this.surface=new THREE.Mesh(resources.surface,new THREE.MeshBasicMaterial({side:THREE.DoubleSide}));this.surface.matrixAutoUpdate=false;
  }
  /** New index views share immutable vertex bytes; dispose only the overlay. */
  highlight(kind:SelectionMode,index:number):THREE.BufferGeometry|null{
    if(this.disposed)return null;
    if(kind==='edge'){
      const range=this.edges.range(this.edgeIds.indexOf(`edge:${index}`));if(!range||range[0]===range[1])return null;
      return new THREE.BufferGeometry().setAttribute('position',new THREE.BufferAttribute(this.buffers.edges.subarray(range[0]*6,range[1]*6),3));
    }
    if(kind==='part'&&!this.buffers.triangles?.length)return new THREE.BufferGeometry().setAttribute('position',new THREE.BufferAttribute(this.buffers.edges,3));
    const range=kind==='part'?[0,this.buffers.triangles?.length/3||0]:this.faces.range(this.faceIds.indexOf(`face:${index}`));
    if(!range||range[0]===range[1])return null;
    return new THREE.BufferGeometry().setAttribute('position',new THREE.BufferAttribute(this.buffers.vertices,3))
      .setIndex(new THREE.BufferAttribute(this.buffers.triangles.subarray(range[0]*3,range[1]*3),1));
  }
  dispose(){if(this.disposed)return;this.disposed=true;this.resources.release();for(const object of [this.lines,this.surface])(object.material as THREE.Material).dispose();}
}
/** Byte identities, view ranges and topology all participate in reuse. Resources
 * can span overlapping scene replacements but leave the pool at their last owner. */
export function scenePicking(){
  const ids=new WeakMap<ArrayBufferLike,number>();let next=0;
  const resources=new Map<string,PickResources>();
  const prepare=(buffers:MeshBuffers,topology?:PickTopology)=>{
    const fields=Object.keys(buffers).sort().map(name=>{const array=buffers[name];let id=ids.get(array.buffer);if(id===undefined){id=next++;ids.set(array.buffer,id);}return [name,array.constructor.name,id,array.byteOffset,array.byteLength];});
    const key=JSON.stringify([fields,topology?.face_ids??null,topology?.edge_ids??null]);
    let shared=resources.get(key);
    if(!shared||shared.released){shared=new PickResources(buffers,topology,()=>{if(resources.get(key)===shared)resources.delete(key);});resources.set(key,shared);}
    return new PickGeometry(buffers,topology,shared);
  };
  return Object.defineProperty(prepare,'size',{get:()=>resources.size}) as typeof prepare & {readonly size:number};
}
export interface PickInstance {instance_id:string;matrix:THREE.Matrix4;geometry:PickGeometry;tolerance_mm?:number}
export interface TopologyHit {instance_id:string;entity_id:string}
/** Pixel thresholds use CSS viewport size, independent of display pixel ratio.
 * Edge occlusion is checked at the candidate point, not the offset mouse ray. */
export function pickTopology(camera:THREE.Camera,ndc:THREE.Vector2,size:{width:number;height:number},mode:SelectionMode,instances:PickInstance[],pixels=6):TopologyHit|null{
  if(size.width<=0||size.height<=0)return null;
  camera.updateMatrixWorld();
  const raycaster=new THREE.Raycaster();raycaster.setFromCamera(ndc,camera);
  const meshes=instances.map(instance=>{instance.geometry.surface.matrixWorld.copy(instance.matrix);return instance.geometry.surface;});
  if(mode!=='edge'){
    const hit=raycaster.intersectObjects(meshes,false).find(hit=>{const z=hit.point.clone().project(camera).z;return hit.faceIndex!=null&&z>=-1&&z<=1;});
    if(hit){const instance=instances.find(instance=>instance.geometry.surface===hit.object);if(instance){const face=instance.geometry.faces.entity(hit.faceIndex!);if(face!==null&&instance.geometry.faceIds[face])return {instance_id:instance.instance_id,entity_id:mode==='part'?'part:0':instance.geometry.faceIds[face]!};}}
    if(mode==='face')return null;
    // Standalone wires have no face mesh but remain selectable as parts.
  }
  const candidates:{instance:PickInstance;edge:number;point:THREE.Vector3;gap:number;depth:number}[]=[];
  const direction=camera.getWorldDirection(new THREE.Vector3());
  for(const instance of instances){
    const line=instance.geometry.lines;
    if(!line.geometry.boundingSphere)line.geometry.computeBoundingSphere();
    const sphere=line.geometry.boundingSphere!.clone().applyMatrix4(instance.matrix);
    const far=sphere.center.clone().addScaledVector(direction,sphere.radius);
    const projected=far.clone().project(camera);
    if(!Number.isFinite(projected.z))continue;
    const adjacent=projected.clone();adjacent.x+=2/size.width;
    raycaster.params.Line.threshold=Math.max(1e-8,adjacent.unproject(camera).distanceTo(far)*pixels);
    line.matrixWorld.copy(instance.matrix);
    for(const hit of raycaster.intersectObject(line,false)){
      const edge=instance.geometry.edges.entity(Math.floor((hit.index??-2)/2));if(edge===null||!instance.geometry.edgeIds[edge])continue;
      const point=hit.point.clone().project(camera);
      if(point.z< -1||point.z>1)continue;
      const gap=Math.hypot((point.x-ndc.x)*size.width/2,(point.y-ndc.y)*size.height/2);
      if(gap<=pixels)candidates.push({instance,edge,point:hit.point,gap,depth:hit.distance});
    }
  }
  candidates.sort((a,b)=>a.gap-b.gap||a.depth-b.depth);
  for(const candidate of candidates){
    const projected=candidate.point.clone().project(camera);
    raycaster.setFromCamera(new THREE.Vector2(projected.x,projected.y),camera);
    const front=raycaster.intersectObjects(meshes,false).find(hit=>{const z=hit.point.clone().project(camera).z;return z>=-1&&z<=1;});
    const distance=raycaster.ray.origin.distanceTo(candidate.point);
    if(front&&front.distance+2*(candidate.instance.tolerance_mm??0.1)<distance)continue;
    return {instance_id:candidate.instance.instance_id,entity_id:mode==='part'?'part:0':candidate.instance.geometry.edgeIds[candidate.edge]!};
  }
  return null;
}
export interface ScreenRect {left:number;top:number;right:number;bottom:number}
function projectCss(point:THREE.Vector3,camera:THREE.Camera,size:{width:number;height:number}){
  const projected=point.clone().project(camera);
  if(!Number.isFinite(projected.z)||projected.z<-1||projected.z>1)return null;
  return {x:(projected.x+1)*size.width/2,y:(1-projected.y)*size.height/2};
}
function pointInRect(x:number,y:number,rect:ScreenRect){
  return x>=rect.left&&x<=rect.right&&y>=rect.top&&y<=rect.bottom;
}
function segmentHitsRect(ax:number,ay:number,bx:number,by:number,rect:ScreenRect){
  if(pointInRect(ax,ay,rect)||pointInRect(bx,by,rect))return true;
  const points:[[number,number],[number,number],[number,number],[number,number]]=[
    [rect.left,rect.top],[rect.right,rect.top],[rect.right,rect.bottom],[rect.left,rect.bottom],
  ];
  for(let i=0;i<4;i++){
    const [cx,cy]=points[i],[dx,dy]=points[(i+1)%4];
    const den=(bx-ax)*(dy-cy)-(by-ay)*(dx-cx);
    if(Math.abs(den)<1e-12)continue;
    const t=((cx-ax)*(dy-cy)-(cy-ay)*(dx-cx))/den;
    const u=((cx-ax)*(by-ay)-(cy-ay)*(bx-ax))/den;
    if(t>=0&&t<=1&&u>=0&&u<=1)return true;
  }
  return false;
}
/** Rectangle selection (Bambu-style): any topology whose projected geometry intersects the CSS rect. */
export function pickTopologyInRect(camera:THREE.Camera,rect:ScreenRect,size:{width:number;height:number},mode:SelectionMode,instances:PickInstance[]):TopologyHit[]{
  if(size.width<=0||size.height<=0||rect.right<rect.left||rect.bottom<rect.top)return [];
  camera.updateMatrixWorld();
  const hits:TopologyHit[]=[];
  const seen=new Set<string>();
  for(const instance of instances){
    const geometry=instance.geometry;
    if(mode==='part'){
      // A rectangle may sit entirely inside a face without covering any vertex.
      // Reuse face/edge intersection so interior coverage still selects the part.
      const faceHits=pickTopologyInRect(camera,rect,size,'face',[instance]);
      const edgeHits=faceHits.length?[]:pickTopologyInRect(camera,rect,size,'edge',[instance]);
      if(faceHits.length||edgeHits.length){
        const key=`${instance.instance_id}:part:0`;
        if(!seen.has(key)){seen.add(key);hits.push({instance_id:instance.instance_id,entity_id:'part:0'});}
      }
      continue;
    }
    if(mode==='face'){
      const vertices=geometry.buffers.vertices,triangles=geometry.buffers.triangles;
      if(!vertices?.length||!triangles?.length)continue;
      for(let face=0;face<geometry.faceIds.length;face++){
        const id=geometry.faceIds[face];if(!id)continue;
        const range=geometry.faces.range(face);if(!range||range[0]===range[1])continue;
        let hit=false;
        const world=new THREE.Vector3();
        for(let primitive=range[0];primitive<range[1]&&!hit;primitive++){
          const points:[number,number][]=[];
          for(let corner=0;corner<3;corner++){
            const index=triangles[primitive*3+corner]*3;
            world.set(vertices[index],vertices[index+1],vertices[index+2]).applyMatrix4(instance.matrix);
            const projected=projectCss(world,camera,size);
            if(!projected){points.length=0;break;}
            if(pointInRect(projected.x,projected.y,rect)){hit=true;break;}
            points.push([projected.x,projected.y]);
          }
          if(!hit&&points.length===3){
            for(let edge=0;edge<3;edge++){
              const [ax,ay]=points[edge],[bx,by]=points[(edge+1)%3];
              if(segmentHitsRect(ax,ay,bx,by,rect)){hit=true;break;}
            }
          }
        }
        if(hit){
          const key=`${instance.instance_id}:${id}`;
          if(!seen.has(key)){seen.add(key);hits.push({instance_id:instance.instance_id,entity_id:id});}
        }
      }
      continue;
    }
    const edges=geometry.buffers.edges;
    if(!edges?.length)continue;
    for(let edge=0;edge<geometry.edgeIds.length;edge++){
      const id=geometry.edgeIds[edge];if(!id)continue;
      const range=geometry.edges.range(edge);if(!range||range[0]===range[1])continue;
      let hit=false;
      for(let segment=range[0];segment<range[1];segment++){
        const i=segment*6;
        const a=new THREE.Vector3(edges[i],edges[i+1],edges[i+2]).applyMatrix4(instance.matrix);
        const b=new THREE.Vector3(edges[i+3],edges[i+4],edges[i+5]).applyMatrix4(instance.matrix);
        const pa=projectCss(a,camera,size),pb=projectCss(b,camera,size);
        if(pa&&pb&&segmentHitsRect(pa.x,pa.y,pb.x,pb.y,rect)){hit=true;break;}
      }
      if(hit){
        const key=`${instance.instance_id}:${id}`;
        if(!seen.has(key)){seen.add(key);hits.push({instance_id:instance.instance_id,entity_id:id});}
      }
    }
  }
  return hits;
}
