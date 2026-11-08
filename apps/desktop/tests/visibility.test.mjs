// Exercise actual pinned dependency methods/materials without WebGL or a browser.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile,mkdtemp,writeFile,rm,mkdir} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import ts from 'typescript';
import {build} from 'esbuild';
import * as THREE from 'three';

test('object visibility hides/restores real viewer materials when names differ from IDs',async()=>{
  const repository=fileURLToPath(new URL('../../../',import.meta.url));
  const vendor=path.join(repository,'node_modules/three-cad-viewer/dist/three-cad-viewer.esm.js');
  const text=await readFile(vendor,'utf8');const parsed=ts.createSourceFile(vendor,text,ts.ScriptTarget.Latest,true);
  const methods={};
  function visit(node){
    if(ts.isBinaryExpression(node)&&ts.isPropertyAccessExpression(node.left)&&node.left.expression.kind===ts.SyntaxKind.ThisKeyword&&['setObject','setState'].includes(node.left.name.text)&&ts.isArrowFunction(node.right))methods[node.left.name.text]=node.right.getText(parsed);
    ts.forEachChild(node,visit);
  }visit(parsed);assert.ok(methods.setObject);assert.ok(methods.setState);
  await mkdir(path.join(repository,'target'),{recursive:true});const temporary=await mkdtemp(path.join(repository,'target/visibility-test-'));
  try{
    // Expose dependency internals only in an ephemeral test module; the installed
    // package and production bundle remain unmodified. This runs its real code.
    const source=text+`\nexport {ObjectGroup,TreeModel}; export function objectApi(){return ${methods.setObject};} export function treeApi(){return ${methods.setState};}`;
    const file=path.join(temporary,'vendor.mjs');await writeFile(file,source);
    const dependency=await import(pathToFileURL(file).href);
    const built=await build({entryPoints:[path.join(repository,'packages/viewer/src/visibility.ts')],bundle:true,platform:'node',format:'esm',write:false});
    const helperFile=path.join(temporary,'helper.mjs');await writeFile(helperFile,built.outputFiles[0].text);
    const {setRenderedVisibility}=await import(pathToFileURL(helperFile).href);
    function part(){
      const group=new dependency.ObjectGroup(.5,1,'#444',{},'solid');
      group.setFront(new THREE.Mesh(new THREE.BufferGeometry(),new THREE.MeshBasicMaterial()));
      group.edgeMaterial=new THREE.LineBasicMaterial();return group;
    }
    const rail=part(),carriage=part();let invalidations=0;
    const receiver={rendered:{nestedGroup:{groups:{'/model/hash-rail':rail,'/model/hash-carriage':carriage},backVisible:false}},idPicker:{setDirty(){invalidations++;}},update(){}};
    const setObject=dependency.objectApi.call(receiver);
    const tree=new dependency.TreeModel({Model:{rail:[1,1],carriage:[1,1]}},{onStateChange(node,icon){setObject('/'+node.path,node.state[icon],icon,false,false);}});
    receiver.rendered.treeview=tree;
    const setState=dependency.treeApi.call(receiver);
    setState('/model/hash-rail',[0,0]);
    assert.equal(rail.front.material.visible,true,'old ID-based tree API call silently leaves the part visible');
    setRenderedVisibility({setObject},'/model/hash-rail',false);
    assert.equal(rail.front.material.visible,false);assert.equal(rail.edgeMaterial.visible,false);assert.equal(rail.getVisibility(),false);
    assert.equal(carriage.front.material.visible,true);assert.equal(carriage.edgeMaterial.visible,true);
    setRenderedVisibility({setObject},'/model/hash-rail',true);
    assert.equal(rail.front.material.visible,true);assert.equal(rail.edgeMaterial.visible,true);assert.equal(rail.getVisibility(),true);assert.equal(invalidations,4);
    rail.front.geometry.dispose();rail.front.material.dispose();rail.edgeMaterial.dispose();carriage.front.geometry.dispose();carriage.front.material.dispose();carriage.edgeMaterial.dispose();
  }finally{await rm(temporary,{recursive:true,force:true});}
});
