// Pinned Three implementation with a counting device. No browser/driver/pixels.
import {WebGLAttributes} from '../node_modules/three-cad-viewer/node_modules/three/src/renderers/webgl/WebGLAttributes.js';
import {WebGLGeometries} from '../node_modules/three-cad-viewer/node_modules/three/src/renderers/webgl/WebGLGeometries.js';
import assert from 'node:assert/strict';

export function renderBufferCounter(){
  let id=0,uploads=0,uploadedBytes=0,peakBytes=0;const live=new Map(),bound=new Map();
  const gl={ARRAY_BUFFER:34962,ELEMENT_ARRAY_BUFFER:34963,FLOAT:5126,UNSIGNED_INT:5125,UNSIGNED_SHORT:5123,UNSIGNED_BYTE:5121,BYTE:5120,SHORT:5122,INT:5124,HALF_FLOAT:5131,
    createBuffer(){const buffer={id:++id};live.set(buffer,0);return buffer;},
    bindBuffer(type,buffer){bound.set(type,buffer);},
    bufferData(type,array){assert.ok(live.has(bound.get(type)));live.set(bound.get(type),array.byteLength);uploads++;uploadedBytes+=array.byteLength;peakBytes=Math.max(peakBytes,[...live.values()].reduce((a,b)=>a+b,0));},
    bufferSubData(){},deleteBuffer(buffer){assert.ok(live.delete(buffer),'device buffer cannot be deleted twice');}};
  const attributes=WebGLAttributes(gl),info={memory:{geometries:0}},geometries=WebGLGeometries(gl,attributes,info,{releaseStatesOfGeometry(){}});
  return {
    draw(root){root.traverse(object=>{
      const geometry=object.geometry;if(!geometry?.isBufferGeometry)return;
      geometries.get(object,geometry);geometries.update(geometry);
      if(geometry.index)attributes.update(geometry.index,gl.ELEMENT_ARRAY_BUFFER);
      object.onBeforeRender({getViewport(target){return target.set(0,0,1600,1000);}});
    });},
    snapshot(){return {upload_calls:uploads,uploaded_bytes:uploadedBytes,peak_live_bytes:peakBytes,live_buffers:live.size,live_bytes:[...live.values()].reduce((a,b)=>a+b,0),registered_geometries:info.memory.geometries};},
  };
}
