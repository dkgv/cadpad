import * as THREE from 'three';

/** Share of the viewport a fitted model should fill. The viewer's own reset frames the bounding sphere, which leaves most parts small. */
const fitFill=0.92;

/** Zoom that places a measured NDC half-extent at `fill` of the view. */
export function framedZoom(zoom:number,ndcHalfExtent:number,fill=fitFill){
  if(!(zoom>0)||!(ndcHalfExtent>0)||!Number.isFinite(ndcHalfExtent))return zoom;
  return Math.min(1e4,zoom*fill/Math.max(ndcHalfExtent,1e-4));
}

/** Largest |NDC| of the box corners under the camera's current projection. */
export function projectedHalfExtent(camera:THREE.Camera,box:THREE.Box3){
  const corner=new THREE.Vector3();
  let span=0;
  camera.updateMatrixWorld();
  for(const x of [box.min.x,box.max.x])for(const y of [box.min.y,box.max.y])for(const z of [box.min.z,box.max.z]){
    corner.set(x,y,z).project(camera);
    if(Number.isFinite(corner.x)&&Number.isFinite(corner.y))span=Math.max(span,Math.abs(corner.x),Math.abs(corner.y));
  }
  return span;
}
