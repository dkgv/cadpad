import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import ts from 'typescript';
import {build} from 'esbuild';
import {WebGLAttributes} from '../../../node_modules/three-cad-viewer/node_modules/three/src/renderers/webgl/WebGLAttributes.js';
import {WebGLGeometries} from '../../../node_modules/three-cad-viewer/node_modules/three/src/renderers/webgl/WebGLGeometries.js';

test('pinned viewer constructs shared render buffers with distinct picking and safe real disposal',async()=>{
  const repository=fileURLToPath(new URL('../../../',import.meta.url)),vendor=path.join(repository,'node_modules/three-cad-viewer/dist/three-cad-viewer.esm.js');
  const text=await fs.readFile(vendor,'utf8'),parsed=ts.createSourceFile(vendor,text,ts.ScriptTarget.Latest,true);
  let render,clear;
  function visit(node){if(ts.isClassDeclaration(node)&&node.name?.text==='Viewer'){render=node.members.find(member=>member.name?.getText(parsed)==='render');clear=node.members.find(member=>member.name?.getText(parsed)==='clear');}ts.forEachChild(node,visit);}visit(parsed);
  assert.ok(render);
  let notification=-1,ready=-1;const paints=[];
  function inspect(node){
    if(ts.isCallExpression(node)&&node.expression.getText(parsed)==='this.notifyCallback'&&node.arguments.some(arg=>ts.isObjectLiteralExpression(arg)&&arg.properties.some(property=>property.name?.getText(parsed)==='target0')))notification=node.pos;
    if(ts.isCallExpression(node)&&node.expression.getText(parsed)==='this.update')paints.push(node.pos);
    if(ts.isBinaryExpression(node)&&node.left.getText(parsed)==='this.ready'&&node.right.kind===ts.SyntaxKind.TrueKeyword)ready=node.pos;
    ts.forEachChild(node,inspect);
  }inspect(render);
  assert.ok(notification>=0&&paints.length&&paints.every(position=>position>notification),'publisher initial paint must follow the target0 notification used for pooling');
  assert.ok(ready>=0&&ready<notification,'publisher groups must be ready before pooling accesses their public getter');
  assert.ok(clear.getText(parsed).includes('this.renderer.clear()'));
  assert.ok(!/this\.renderer\.(dispose|forceContextLoss)\(/.test(clear.getText(parsed)),'scene replacement must preserve the renderer/context for upload reuse');
  const directory=await fs.mkdtemp(path.join(repository,'target/render-resources-'));
  try{
    await fs.writeFile(path.join(directory,'vendor.mjs'),text+'\nexport {NestedGroup};');
    const {NestedGroup}=await import(pathToFileURL(path.join(directory,'vendor.mjs')).href);
    const result=await build({entryPoints:[path.join(repository,'packages/viewer/src/render-buffers.ts')],bundle:true,platform:'node',format:'esm',write:false});
    await fs.writeFile(path.join(directory,'helper.mjs'),result.outputFiles[0].text);
    const {shareRenderBuffers,revisionRenderBuffers}=await import(pathToFileURL(path.join(directory,'helper.mjs')).href);
    const shape={vertices:new Float32Array([0,0,0,10,0,0,0,10,0]),normals:new Float32Array([0,0,1,0,0,1,0,0,1]),triangles:new Uint32Array([0,1,2]),triangles_per_face:new Uint32Array([1]),edges:new Float32Array([0,0,0,10,0,0]),segments_per_edge:new Uint32Array([1])};
    const rawSnapshot=()=>Object.fromEntries(Object.entries(shape).map(([name,array])=>[name,Array.from(array)]));
    const originalBytes=rawSnapshot();
    const tree={version:3,id:'/model',name:'Model',loc:[[0,0,0],[0,0,0,1]],parts:[0,1].map(i=>({version:3,id:`/model/id-${i}`,name:`part-${i}`,type:'shapes',subtype:'solid',shape,state:[1,1],color:i?'#ff0000':'#0000ff',alpha:1,loc:[[i*20,0,0],[0,0,0,1]]}))};
    const nested=new NestedGroup(tree,800,600,'#666666',false,1,.15,.65,0);nested.assignIds=true;nested.render();
    const a=nested.groups['/model/id-0'],b=nested.groups['/model/id-1'];
    const faceIds=[a.front.geometry.attributes.componentId,b.front.geometry.attributes.componentId];
    assert.notEqual(faceIds[0].array[0],faceIds[1].array[0]);
    const pool=shareRenderBuffers([a,b]);
    assert.equal(a.front.geometry.attributes.position,b.front.geometry.attributes.position);
    assert.equal(a.front.geometry.index,b.front.geometry.index);
    assert.equal(a.edges.geometry.attributes.instanceStart,b.edges.geometry.attributes.instanceStart);
    assert.equal(a.front.geometry.attributes.componentId,faceIds[0]);assert.equal(b.front.geometry.attributes.componentId,faceIds[1]);
    assert.notEqual(a.front.material,b.front.material);assert.equal(b.position.x,20);
    let next=0;const live=new Set(),gl={ARRAY_BUFFER:34962,ELEMENT_ARRAY_BUFFER:34963,FLOAT:5126,UNSIGNED_INT:5125,UNSIGNED_SHORT:5123,
      createBuffer(){const buffer={id:++next};live.add(buffer);return buffer;},bindBuffer(){},bufferData(){},bufferSubData(){},deleteBuffer(buffer){assert.ok(live.delete(buffer),'do not double-free a shared buffer');}};
    const attributes=WebGLAttributes(gl),info={memory:{geometries:0}},geometries=WebGLGeometries(gl,attributes,info,{releaseStatesOfGeometry(){}});
    const draw=group=>group.traverse(object=>{if(!object.geometry)return;const geometry=object.geometry;geometries.get(object,geometry);geometries.update(geometry);if(geometry.index)attributes.update(geometry.index,gl.ELEMENT_ARRAY_BUFFER);object.onBeforeRender({getViewport(target){return target.set(0,0,800,600);}});});
    draw(a);draw(b);const buffer=attributes.get(b.front.geometry.attributes.position).buffer;
    a.front.geometry.dispose();assert.ok(live.has(buffer));draw(a);assert.equal(attributes.get(a.front.geometry.attributes.position).buffer,buffer);
    a.setShapeVisible(false);assert.equal(a.front.material.visible,false);assert.equal(b.front.material.visible,true);a.setShapeVisible(true);
    pool.dispose();nested.dispose();assert.equal(live.size,0);assert.equal(info.memory.geometries,0);
    assert.deepEqual(rawSnapshot(),originalBytes,'pinned construction/render-buffer setup/disposal must preserve cached raw arrays');
    const storage=revisionRenderBuffers();let previous=null,previousPool=null,firstBuffer=null;
    for(let revision=0;revision<12;revision++){
      if(previous){storage.beginSwap();previousPool.dispose();previous.dispose();}
      const nextTree={...tree,parts:tree.parts.map((part,index)=>({...part,id:`/model/rev-${revision}-${index}`,loc:[[index*20+revision,0,0],[0,0,0,1]]}))};
      const next=new NestedGroup(nextTree,800,600,'#666666',false,1,.15,.65,0);next.assignIds=true;next.render();
      const groups=nextTree.parts.map(part=>next.groups[part.id]),incoming=storage.attach(groups);storage.finishSwap();
      for(const group of groups)draw(group);
      const uploaded=attributes.get(groups[0].front.geometry.attributes.position).buffer;
      if(firstBuffer)assert.equal(uploaded,firstBuffer,'actual pinned groups retain the original static device buffer');else firstBuffer=uploaded;
      assert.equal(groups[0].position.x,revision);assert.equal(storage.stats.retainedGeometries,0);
      previous=next;previousPool=incoming;
    }
    previousPool.dispose();previous.dispose();storage.dispose();assert.equal(live.size,0);assert.equal(info.memory.geometries,0);
    assert.deepEqual(rawSnapshot(),originalBytes);
    // A worker-published wire uses the same solid-shaped transport envelope,
    // with empty face arrays and real edge spans. Exercise the pinned viewer's
    // construction/disposal without a browser or a graphics context.
    const wireShape={vertices:new Float32Array(),normals:new Float32Array(),triangles:new Uint32Array(),triangles_per_face:new Uint32Array(),edges:new Float32Array([0,0,0,5,0,0,5,0,0,10,0,0]),segments_per_edge:new Uint32Array([2]),edge_types:[0],obj_vertices:new Float32Array([0,0,0,10,0,0])};
    const wireTree={...tree,parts:[{...tree.parts[0],id:'/model/wire',name:'wire',shape:wireShape}]};
    const wires=new NestedGroup(wireTree,800,600,'#666666',false,1,.15,.65,0);wires.assignIds=true;wires.render();
    const wire=wires.groups['/model/wire'];
    assert.equal(wire.edges.geometry.attributes.instanceStart.count,2);
    draw(wire);assert.ok(live.size>0);
    wires.dispose();assert.equal(live.size,0);assert.equal(info.memory.geometries,0);
  }finally{await fs.rm(directory,{recursive:true,force:true});}
});
