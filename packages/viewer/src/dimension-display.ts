import * as THREE from 'three';

export interface LabelBox {x:number;y:number;w:number;h:number}
export interface LabelRequest extends LabelBox {depth:number;offset:[number,number]}
/** Input order is priority order. Try a bounded set of nearby positions, then
 * suppress; never move a label beyond the canvas or cover a higher priority one. */
export function placeDimensionLabels(requests:LabelRequest[],width:number,height:number):(LabelBox|null)[]{
  const occupied:LabelBox[]=[];
  return requests.map(request=>{
    if(!Object.values(request).flat().every(Number.isFinite)||request.depth< -1||request.depth>1||width<=0||height<=0)return null;
    for(const [dx,dy] of [[0,0],[0,-26],[0,26],[0,-52],[0,52]]){
      const box={x:request.x+request.offset[0]+dx,y:request.y+request.offset[1]+dy,w:request.w,h:request.h};
      if(box.x-box.w/2<0||box.y-box.h/2<0||box.x+box.w/2>width||box.y+box.h/2>height)continue;
      if(occupied.some(b=>Math.abs(b.x-box.x)<(b.w+box.w)/2+5&&Math.abs(b.y-box.y)<(b.h+box.h)/2+4))continue;
      occupied.push(box);return box;
    }
    return null;
  });
}

export interface DimensionSegment {start:THREE.Vector3;end:THREE.Vector3;arrows?:boolean|'start'|'end';clampHeads?:boolean}
export function moveDimensionStroke(mesh:ReturnType<typeof dimensionStroke>,segments:DimensionSegment[]){
  const positions=mesh.geometry.getAttribute('position'),peers=mesh.geometry.getAttribute('peer'),ends=mesh.geometry.getAttribute('endpoint');
  let index=0;
  for(const segment of segments)for(let vertex=0;vertex<(6+(segment.arrows===true?6:segment.arrows?3:0));vertex++,index++){
    const [base,peer]=ends.getX(index)?[segment.end,segment.start]:[segment.start,segment.end];
    positions.setXYZ(index,base.x,base.y,base.z);peers.setXYZ(index,peer.x,peer.y,peer.z);
  }
  positions.needsUpdate=true;peers.needsUpdate=true;
}
// The vertex shader expands triangle strokes in CSS pixels *after* camera
// projection. Unlike native GL lines, width does not depend on driver limits,
// world scale or devicePixelRatio. Depth testing is intentionally disabled:
// these are x-ray reference dimensions, not hidden-line model edges.
export function dimensionStroke(segments:DimensionSegment[]):THREE.Mesh<THREE.BufferGeometry,THREE.ShaderMaterial>{
  const positions:number[]=[],peers:number[]=[],offsets:number[]=[],ends:number[]=[],clamps:number[]=[];
  const vertex=(base:THREE.Vector3,peer:THREE.Vector3,along:number,across:number,end:number)=>{
    positions.push(...base.toArray());peers.push(...peer.toArray());offsets.push(along,across);ends.push(end);
  };
  for(const {start:a,end:b,arrows,clampHeads=true} of segments){
    const count=6+(arrows===true?6:arrows?3:0);for(let i=0;i<count;i++)clamps.push(Number(clampHeads));
    // Both endpoint frames point toward their peer; invert the far normal.
    for(const [base,peer,x,y,end] of [[a,b,0,-.65,0],[b,a,0,.65,1],[a,b,0,.65,0],[a,b,0,.65,0],[b,a,0,.65,1],[b,a,0,-.65,1]] as const)vertex(base,peer,x,y,end);
    if(arrows)for(const [base,peer,end] of [[a,b,0],[b,a,1]] as const){
      if(arrows==='start'&&end===1||arrows==='end'&&end===0)continue;
      vertex(base,peer,0,0,end);vertex(base,peer,8,3,end);vertex(base,peer,8,-3,end);
    }
  }
  const geometry=new THREE.BufferGeometry();
  geometry.setAttribute('position',new THREE.Float32BufferAttribute(positions,3));
  geometry.setAttribute('peer',new THREE.Float32BufferAttribute(peers,3));
  geometry.setAttribute('pixelOffset',new THREE.Float32BufferAttribute(offsets,2));
  geometry.setAttribute('endpoint',new THREE.Float32BufferAttribute(ends,1));
  geometry.setAttribute('clampHead',new THREE.Float32BufferAttribute(clamps,1));
  const material=new THREE.ShaderMaterial({depthTest:false,depthWrite:false,transparent:true,side:THREE.DoubleSide,forceSinglePass:true,
    uniforms:{resolution:{value:new THREE.Vector2(1,1)},endOffset:{value:new THREE.Vector2()},enabled:{value:1},width:{value:1},color:{value:new THREE.Color('#256d80')}},
    vertexShader:`
      attribute vec3 peer;
      attribute vec2 pixelOffset;
      attribute float endpoint;
      attribute float clampHead;
      uniform vec2 resolution;
      uniform vec2 endOffset;
      uniform float enabled;
      uniform float width;
      void main(){
        vec4 base=projectionMatrix*modelViewMatrix*vec4(position,1.0);
        vec4 other=projectionMatrix*modelViewMatrix*vec4(peer,1.0);
        base.xy+=endpoint*endOffset*2.0/resolution*base.w;
        other.xy+=(1.0-endpoint)*endOffset*2.0/resolution*other.w;
        if(enabled<0.5||base.w<=0.0||other.w<=0.0||abs(base.z)>base.w||abs(other.z)>other.w){gl_Position=vec4(2.0,2.0,2.0,1.0);return;}
        vec2 delta=(other.xy/other.w-base.xy/base.w)*resolution*0.5;
        float distance=length(delta);
        vec2 direction=delta/max(distance,0.00001);
        vec2 offset=direction*mix(pixelOffset.x,min(pixelOffset.x,distance*0.35),clampHead)+vec2(-direction.y,direction.x)*pixelOffset.y*width;
        base.xy+=offset*2.0/resolution*base.w;
        gl_Position=base;
      }`,
    fragmentShader:`uniform vec3 color;
      void main(){
        gl_FragColor=vec4(color,0.85);
        #include <colorspace_fragment>
      }`});
  const mesh=new THREE.Mesh(geometry,material);mesh.frustumCulled=false;mesh.renderOrder=100;mesh.raycast=()=>{};
  mesh.onBeforeRender=renderer=>{renderer.getSize(material.uniforms.resolution.value);};
  return mesh;
}


/** Unsigned three-point angles in radians. A fixed segment count allows motion
 * to update the same attributes, including transitions through a zero angle. */
export function angleGeometry(start:THREE.Vector3,vertex:THREE.Vector3,end:THREE.Vector3,previousNormal?:THREE.Vector3){
  const a=start.clone().sub(vertex),b=end.clone().sub(vertex),la=a.length(),lb=b.length();
  const valid=Number.isFinite(la+lb)&&Math.min(la,lb)>1e-9;
  if(valid){a.divideScalar(la);b.divideScalar(lb);}else{a.set(1,0,0);b.copy(a);}
  const normal=new THREE.Vector3().crossVectors(a,b),sine=normal.length();
  const value=valid?Math.atan2(sine,a.dot(b)):null;
  if(sine<1e-12){
    // Collinear rays have no unique plane. Keep the last defined plane where
    // possible, projected perpendicular to the current first ray. Never use
    // history to override the plane of a non-collinear unsigned angle.
    if(previousNormal)normal.copy(previousNormal).addScaledVector(a,-previousNormal.dot(a));
    if(!Number.isFinite(normal.lengthSq())||normal.lengthSq()<1e-24)normal.crossVectors(a,Math.abs(a.x)<.8?new THREE.Vector3(1,0,0):new THREE.Vector3(0,1,0));
  }
  normal.normalize();
  const tangent=new THREE.Vector3().crossVectors(normal,a),radius=valid?Math.min(la,lb)*.3:0;
  const point=(t:number)=>vertex.clone().addScaledVector(a,Math.cos(t)*radius).addScaledVector(tangent,Math.sin(t)*radius);
  const segments:DimensionSegment[]=[];
  for(let i=0;i<64;i++)segments.push({start:point((value||0)*i/64),end:point((value||0)*(i+1)/64),arrows:i===0?'start':i===63?'end':false,clampHeads:false});
  // The retained plane may differ from a nearly collinear ray by rounding
  // noise. Keep the arc endpoint attached to the actual measured ray.
  segments[63].end=vertex.clone().addScaledVector(b,radius);
  segments.push({start:vertex,end:start},{start:vertex,end});
  return {segments,anchor:point((value||0)/2),normal,value,valid};
}

export function dimensionText(annotation:{label:string;kind:string;value?:number;parameter?:string|null;format?:{unit:'mm'|'cm'|'m'|'in'|'deg'|'rad';precision:number}}){
  if(!Number.isFinite(annotation.value))return `${annotation.label} · needs repair`;
  const angular=annotation.kind==='angle';
  const format=annotation.format||{unit:angular?'deg':'mm',precision:2};
  if(angular!==(format.unit==='deg'||format.unit==='rad'))return `${annotation.label} · needs repair`;
  const factor={mm:1,cm:10,m:1000,in:25.4,deg:Math.PI/180,rad:1}[format.unit];
  const mark=annotation.kind==='diameter'?'⌀ ':annotation.kind==='radius'?'R ':'';
  return `${annotation.label} ${mark}${(annotation.value!/factor).toFixed(Math.max(0,Math.min(6,format.precision)))} ${format.unit}${annotation.parameter?' ↗':''}`;
}

interface PlacementInput {
  kind:string;part:string;end_part?:string;vertex_part?:string;
  start:[number,number,number];end:[number,number,number];vertex?:[number,number,number];value:number;
  definition?:{kind:string;axis?:'x'|'y'|'z'};
}
export function placeAnnotation(annotation:PlacementInput,transforms:Map<string,THREE.Matrix4|undefined>,localAngleLayouts:WeakMap<PlacementInput,ReturnType<typeof angleGeometry>>){
  const transform=transforms.get(annotation.part),endTransform=transforms.get(annotation.end_part||annotation.part);
  if(!transform||!endTransform)return null;
  const start=new THREE.Vector3(...annotation.start).applyMatrix4(transform),end=new THREE.Vector3(...annotation.end).applyMatrix4(endTransform);
  if(annotation.kind==='angle'){
    const vertexTransform=transforms.get(annotation.vertex_part||annotation.part);if(!annotation.vertex||!vertexTransform)return null;
    const samePart=annotation.part===(annotation.end_part||annotation.part)&&annotation.part===(annotation.vertex_part||annotation.part);
    // Construct every arc in its vertex part's local frame. This makes both
    // the initial fallback plane and the retained plane follow rigid motion.
    let local=localAngleLayouts.get(annotation);
    if(samePart){
      if(!local){local=angleGeometry(new THREE.Vector3(...annotation.start),new THREE.Vector3(...annotation.vertex),new THREE.Vector3(...annotation.end));localAngleLayouts.set(annotation,local);}
    }else{
      const inverse=vertexTransform.clone().invert();
      local=angleGeometry(start.clone().applyMatrix4(inverse),new THREE.Vector3(...annotation.vertex),end.clone().applyMatrix4(inverse),local?.normal);
      // A collapsed ray cannot define a new plane; preserve the last valid one
      // so a transient invalid pose does not relocate the recovered label.
      if(local.valid)localAngleLayouts.set(annotation,local);
    }
    const arc={...local,segments:local.segments.map(segment=>({...segment,start:segment.start.clone().applyMatrix4(vertexTransform),end:segment.end.clone().applyMatrix4(vertexTransform)})),anchor:local.anchor.clone().applyMatrix4(vertexTransform),normal:local.normal.clone().transformDirection(vertexTransform)};
    return {...arc,value:arc.valid?(samePart?annotation.value:arc.value??undefined):undefined};
  }
  const value=annotation.part===(annotation.end_part||annotation.part)?annotation.value:start.distanceTo(end);
  const segments:DimensionSegment[]=[];
  if(annotation.kind==='overall'){
    const axis=annotation.definition?.kind==='overall'?'xyz'.indexOf(annotation.definition.axis||'x'):0;
    const offset=new THREE.Vector3().setComponent((axis+1)%3,-1).transformDirection(transform).multiplyScalar(8);
    segments.push({start:start.clone(),end:start.clone().add(offset)},{start:end.clone(),end:end.clone().add(offset)});
    start.add(offset);end.add(offset);
  }
  segments.unshift({start,end,arrows:true});
  return {segments,anchor:start.clone().add(end).multiplyScalar(.5),value,valid:start.distanceTo(end)>1e-12};
}
