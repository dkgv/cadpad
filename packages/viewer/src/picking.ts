import * as THREE from 'three';
import {MeshBVH,LineSegmentsBVH} from 'three-mesh-bvh';

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
/** Matches the STL crease angle that decides which edges are drawn. */
const FILL_CREASE_DOT=Math.cos(25*Math.PI/180);
/** Position grid for welding duplicated vertices (0.1 µm in model millimetres). */
const FILL_WELD=1e4;
/**
 * Triangle adjacency for "fill to edges" selection. Two triangles connect when
 * they share exactly one welded edge, that edge is not drawn, and the facets
 * meet at less than the crease angle. Drawn edges and creases bound a region.
 */
export class FillTopology {
  /** Three slots per triangle: the neighbouring triangle across that side, or -1. */
  readonly neighbors:Int32Array;
  readonly triangleFace:Int32Array;
  constructor(buffers:MeshBuffers,faces:PrimitiveSpans){
    const vertices=buffers.vertices??new Float32Array(),triangles=buffers.triangles??new Uint32Array(),edges=buffers.edges??new Float32Array();
    const count=triangles.length/3;
    const ids=new Map<string,number>(),cached=new Int32Array(vertices.length/3).fill(-1);
    const weld=(x:number,y:number,z:number)=>{
      // `+0` folds -0 into 0 so mirrored zero coordinates still meet.
      const key=`${Math.round(x*FILL_WELD)+0},${Math.round(y*FILL_WELD)+0},${Math.round(z*FILL_WELD)+0}`;
      let id=ids.get(key);if(id===undefined){id=ids.size;ids.set(key,id);}return id;
    };
    const corners=new Int32Array(triangles.length);
    for(let i=0;i<triangles.length;i++){
      const vertex=triangles[i];
      if(cached[vertex]<0)cached[vertex]=weld(vertices[vertex*3],vertices[vertex*3+1],vertices[vertex*3+2]);
      corners[i]=cached[vertex];
    }
    const wallCorners=new Int32Array(edges.length/3);
    for(let i=0;i<wallCorners.length;i++)wallCorners[i]=weld(edges[i*3],edges[i*3+1],edges[i*3+2]);
    const span=ids.size+1;
    const key=(a:number,b:number)=>a<b?a*span+b:b*span+a;
    const walls=new Set<number>();
    for(let i=0;i+1<wallCorners.length;i+=2)walls.add(key(wallCorners[i],wallCorners[i+1]));
    const normals=new Float64Array(count*3);
    for(let t=0;t<count;t++){
      const [a,b,c]=[triangles[t*3]*3,triangles[t*3+1]*3,triangles[t*3+2]*3];
      const ux=vertices[b]-vertices[a],uy=vertices[b+1]-vertices[a+1],uz=vertices[b+2]-vertices[a+2];
      const vx=vertices[c]-vertices[a],vy=vertices[c+1]-vertices[a+1],vz=vertices[c+2]-vertices[a+2];
      const nx=uy*vz-uz*vy,ny=uz*vx-ux*vz,nz=ux*vy-uy*vx,length=Math.hypot(nx,ny,nz);
      if(length>0){normals[t*3]=nx/length;normals[t*3+1]=ny/length;normals[t*3+2]=nz/length;}
    }
    // First slot to claim each edge; -1 once a third triangle makes it non-manifold.
    const owner=new Map<number,number>(),slotKey=(slot:number)=>key(corners[slot],corners[slot-slot%3+(slot%3+1)%3]);
    const neighbors=new Int32Array(count*3).fill(-1),partner=new Int32Array(count*3).fill(-1);
    for(let slot=0;slot<count*3;slot++){
      const edge=slotKey(slot);
      if(corners[slot]===corners[slot-slot%3+(slot%3+1)%3])continue;
      const first=owner.get(edge);
      if(first===undefined){owner.set(edge,slot);continue;}
      if(first<0)continue;
      if(partner[first]>=0){
        const second=partner[first];
        neighbors[first]=neighbors[second]=partner[first]=partner[second]=-1;
        owner.set(edge,-1);continue;
      }
      partner[first]=slot;partner[slot]=first;
      neighbors[first]=Math.floor(slot/3);neighbors[slot]=Math.floor(first/3);
    }
    for(let slot=0;slot<count*3;slot++){
      const other=neighbors[slot];if(other<0)continue;
      const self=Math.floor(slot/3);
      const dot=normals[self*3]*normals[other*3]+normals[self*3+1]*normals[other*3+1]+normals[self*3+2]*normals[other*3+2];
      const degenerate=!normals[self*3]&&!normals[self*3+1]&&!normals[self*3+2]||!normals[other*3]&&!normals[other*3+1]&&!normals[other*3+2];
      // STL winding is not reliable, so compare planes rather than orientation.
      if(walls.has(slotKey(slot))||!degenerate&&Math.abs(dot)<FILL_CREASE_DOT)neighbors[slot]=-1;
    }
    const triangleFace=new Int32Array(count).fill(-1);
    for(let face=0;face<faces.ends.length;face++){const range=faces.range(face)!;triangleFace.fill(face,range[0],range[1]);}
    this.neighbors=neighbors;this.triangleFace=triangleFace;
  }
  /** Face indices reachable from `face` without crossing a drawn edge or crease, seed first. */
  region(face:number,faces:PrimitiveSpans):number[]{
    const seed=faces.range(face);if(!seed)return [];
    const seen=new Set([face]),order=[face];
    for(let next=0;next<order.length;next++){
      const range=faces.range(order[next])!;
      for(let slot=range[0]*3;slot<range[1]*3;slot++){
        const triangle=this.neighbors[slot];if(triangle<0)continue;
        const other=this.triangleFace[triangle];
        if(other>=0&&!seen.has(other)){seen.add(other);order.push(other);}
      }
    }
    return order;
  }
}
/** Immutable CPU-only topology and geometry; each occurrence owns its raycast pose. */
class PickResources {
  readonly faces:PrimitiveSpans;
  readonly edges:PrimitiveSpans;
  readonly faceIds:readonly (string|null)[];
  readonly edgeIds:readonly (string|null)[];
  readonly surface:THREE.BufferGeometry;
  readonly lines:THREE.BufferGeometry;
  // Build on first use. Indirection preserves published primitive order.
  private surfaceIndex?:MeshBVH;
  private lineIndex?:LineSegmentsBVH;
  get surfaceTree(){
    return this.surfaceIndex??=new MeshBVH(this.surface,{indirect:true});
  }
  get lineTree(){
    return this.lineIndex??=new LineSegmentsBVH(this.lines,{indirect:true});
  }
  private fillIndex?:FillTopology;
  get fill(){
    return this.fillIndex??=new FillTopology(this.buffers,this.faces);
  }
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
      const pattern=kind==='face'?/^face:[0-9]+$/:/^edge:[0-9]+$/;
      const seen=new Set<string>();
      if(ids.length!==spans.ends.length||ids.some(id=>{
        if(id===null)return false;
        if(!pattern.test(id)||seen.has(id))return true;
        seen.add(id);return false;
      }))throw new Error('Invalid display topology identities');
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
    if(this.buffers.triangles?.length>=384)this.surface.raycast=(raycaster,hits)=>resources.surfaceTree.raycastObject3D(this.surface,raycaster,hits);
    if(this.buffers.edges?.length>=768)this.lines.raycast=(raycaster,hits)=>resources.lineTree.raycastObject3D(this.lines,raycaster,hits);
  }
  /** Face ids sharing the drawn-edge-bounded region of `entity_id`, seed first. */
  fillRegion(entity_id:string):string[]{
    const face=this.faceIds.indexOf(entity_id);
    if(this.disposed||face<0||!this.buffers.triangles?.length)return face<0?[]:[entity_id];
    return this.resources.fill.region(face,this.faces).flatMap(index=>this.faceIds[index]?[this.faceIds[index]!]:[]);
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
/** Center of the primitives this geometry draws. Face overlays share the part
 * vertex buffer and only subset the index, so attribute bounds are the whole part. */
export function geometryCenter(geometry:THREE.BufferGeometry){
  const position=geometry.getAttribute('position');
  const index=geometry.getIndex();
  if(!position)return new THREE.Vector3();
  if(!index){geometry.computeBoundingBox();return geometry.boundingBox!.getCenter(new THREE.Vector3());}
  const box=new THREE.Box3(),point=new THREE.Vector3();
  for(let i=0;i<index.count;i++)box.expandByPoint(point.fromBufferAttribute(position,index.getX(i)));
  return box.isEmpty()?new THREE.Vector3():box.getCenter(point);
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
  const nearDepth=new THREE.Vector3(0,0,-1).unproject(camera).dot(direction);
  const farDepth=new THREE.Vector3(0,0,1).unproject(camera).dot(direction);
  const endpoint=new THREE.Vector3();
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
      const segment=Math.floor((hit.index??-2)/2);
      const edge=instance.geometry.edges.entity(segment);if(edge===null||!instance.geometry.edgeIds[edge])continue;
      const depth=hit.point.dot(direction);
      const clippedDepth=THREE.MathUtils.clamp(depth,nearDepth,farDepth);
      if(clippedDepth!==depth){
        // The closest point on the full segment can lie past a clip plane.
        // Try the visible endpoint where the segment crosses that plane.
        const positions=line.geometry.getAttribute('position');
        const reachesPlane=(vertex:number)=>{
          endpoint.fromBufferAttribute(positions,vertex).applyMatrix4(instance.matrix);
          const offset=endpoint.dot(direction)-depth;
          const fraction=(clippedDepth-depth)/offset;
          if(fraction<0||fraction>1)return false;
          hit.point.lerp(endpoint,fraction);
          return true;
        };
        if(!reachesPlane(segment*2)&&!reachesPlane(segment*2+1))continue;
      }
      const point=hit.point.clone().project(camera);
      const gap=Math.hypot((point.x-ndc.x)*size.width/2,(point.y-ndc.y)*size.height/2);
      if(gap<=pixels)candidates.push({instance,edge,point:hit.point,gap,depth:clippedDepth});
    }
  }
  candidates.sort((a,b)=>a.gap-b.gap||a.depth-b.depth);
  const normalMatrix=new THREE.Matrix3(),normal=new THREE.Vector3();
  for(const candidate of candidates){
    const projected=candidate.point.clone().project(camera);
    raycaster.setFromCamera(new THREE.Vector2(projected.x,projected.y),camera);
    const distance=raycaster.ray.origin.distanceTo(candidate.point);
    const tolerance=2*(candidate.instance.tolerance_mm??0.1);
    const occluded=raycaster.intersectObjects(meshes,false).some(hit=>{
      const gap=distance-hit.distance;
      if(gap<=tolerance)return false;
      const z=hit.point.clone().project(camera).z;
      if(z< -1||z>1)return false;
      if(hit.object!==candidate.instance.geometry.surface||!hit.face)return true;
      // Tessellation error is measured perpendicular to the face. Along a
      // grazing ray the same error can span a much larger depth interval.
      normal.copy(hit.face.normal).applyNormalMatrix(normalMatrix.getNormalMatrix(hit.object.matrixWorld));
      return gap*Math.abs(normal.dot(raycaster.ray.direction))>tolerance;
    });
    if(occluded)continue;
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
