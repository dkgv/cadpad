import test from 'node:test';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import * as THREE from 'three';
import {PickGeometry,pickTopology,type MeshBuffers} from '../src/picking.ts';

test('real CAD tessellation picks analytic faces and edges with exact semantic properties after motion',()=>{
  const python=fileURLToPath(new URL(process.platform==='win32'?'../../../.venv/Scripts/python.exe':'../../../.venv/bin/python',import.meta.url));
  const child=spawnSync(python,['-c',`
import json, math, struct, tempfile
from pathlib import Path
from cadpad_worker.model import Model
with tempfile.TemporaryDirectory() as directory:
    root=Path(directory); source=root/'source'; source.mkdir()
    (source/'model.py').write_text('import cadquery as cq\\nshape=cq.Workplane().box(10,20,6)\\nshow(shape,name="plate",features={"top":shape.faces(">Z"),"top_border":shape.faces(">Z").edges()})')
    model=Model(root/'cache'); part=model.build(dict(source_root=str(source)),"r1")["parts"][0]
    buffers={}
    for name, info in part['mesh']['buffers'].items():
        data=(model.cache/info['handle']).read_bytes()
        code='I' if name in ('triangles','triangles_per_face','segments_per_edge','face_types','edge_types') else 'f'
        buffers[name]=struct.unpack('<'+str(len(data)//4)+code,data)
    refs=[dict(revision='r1',instance_id='plate',entity_id=f'{kind}:{index}') for kind in ('face','edge') for index in range(part[kind+'_count'])]
    exact=model.inspect(dict(entities=refs))['entities']
    posed=model.inspect(dict(entities=refs,poses={'plate':dict(position=[5,0,0],quaternion=[0,0,math.sqrt(0.5),math.sqrt(0.5)])}))['entities']
    print(json.dumps(dict(buffers=buffers,exact=exact,posed=posed)))
`],{encoding:'utf8',timeout:60000,maxBuffer:4*1024*1024});
  assert.equal(child.status,0,`${child.error||''}\n${child.stderr}`);
  const data=JSON.parse(child.stdout),buffers:MeshBuffers={};
  for(const [name,values] of Object.entries(data.buffers))buffers[name]=['triangles','triangles_per_face','segments_per_edge','face_types','edge_types'].includes(name)?new Uint32Array(values as number[]):new Float32Array(values as number[]);
  const geometry=new PickGeometry(buffers),instance={instance_id:'plate',matrix:new THREE.Matrix4(),geometry};
  const camera=new THREE.OrthographicCamera(-20,20,20,-20,0.1,100);camera.position.set(0,0,50);camera.lookAt(0,0,0);camera.updateMatrixWorld();
  const size={width:400,height:400};
  const face=pickTopology(camera,new THREE.Vector2(),size,'face',[instance])!;
  assert.ok(face);
  const properties=data.exact.find((item:{reference:{entity_id:string}})=>item.reference.entity_id===face.entity_id);
  assert.equal(properties.surface_type,'PLANE');assert.ok(Math.abs(properties.area-200)<1e-9);assert.deepEqual(properties.semantic_labels,['top']);assert.deepEqual(properties.normal,[0,0,1]);
  const edge=pickTopology(camera,new THREE.Vector2(0,0.49),size,'edge',[instance])!;
  assert.ok(edge);
  const edgeProperties=data.exact.find((item:{reference:{entity_id:string}})=>item.reference.entity_id===edge.entity_id);
  assert.equal(edgeProperties.curve_type,'LINE');assert.equal(edgeProperties.length,10);assert.deepEqual(edgeProperties.semantic_labels,['top_border']);
  instance.matrix.makeRotationZ(Math.PI/2).setPosition(5,0,0);
  const moved=pickTopology(camera,new THREE.Vector2(-0.24,0),size,'edge',[instance]);
  assert.deepEqual(moved,edge);
  const posed=data.posed.find((item:{reference:{entity_id:string}})=>item.reference.entity_id===edge.entity_id);
  assert.ok(Math.abs(posed.world_center[0]+5)<1e-10);assert.ok(Math.abs(posed.world_center[1])<1e-10);assert.equal(posed.length,10);
  geometry.dispose();
});

test('real sphere pole spans map only the published nondegenerate CAD edge',()=>{
  const python=fileURLToPath(new URL(process.platform==='win32'?'../../../.venv/Scripts/python.exe':'../../../.venv/bin/python',import.meta.url));
  const child=spawnSync(python,['-c',`
import json, struct, tempfile
from pathlib import Path
from cadpad_worker.model import Model
with tempfile.TemporaryDirectory() as directory:
    root=Path(directory); source=root/'source'; source.mkdir()
    (source/'model.py').write_text('import cadquery as cq\\nshow(cq.Workplane().sphere(25),name="sphere")')
    model=Model(root/'cache'); part=model.build(dict(source_root=str(source)),"r1")["parts"][0]
    buffers={}
    for name, info in part['mesh']['buffers'].items():
        data=(model.cache/info['handle']).read_bytes()
        code='I' if name in ('triangles','triangles_per_face','segments_per_edge','face_types','edge_types') else 'f'
        buffers[name]=struct.unpack('<'+str(len(data)//4)+code,data)
    exact=model.inspect(dict(entities=[dict(revision='r1',instance_id='sphere',entity_id='edge:0')]))['entities'][0]
    print(json.dumps(dict(buffers=buffers,mesh=part['mesh'],edge_count=part['edge_count'],exact=exact)))
`],{encoding:'utf8',timeout:60000,maxBuffer:4*1024*1024});
  assert.equal(child.status,0,`${child.error||''}\n${child.stderr}`);
  const data=JSON.parse(child.stdout),buffers:MeshBuffers={};
  for(const [name,values] of Object.entries(data.buffers))buffers[name]=['triangles','triangles_per_face','segments_per_edge','face_types','edge_types'].includes(name)?new Uint32Array(values as number[]):new Float32Array(values as number[]);
  assert.equal(data.edge_count,1);assert.deepEqual(data.mesh.edge_ids,[null,'edge:0',null]);
  const geometry=new PickGeometry(buffers,data.mesh),camera=new THREE.OrthographicCamera(-50,50,50,-50,0.1,200);
  camera.position.set(0,0,100);camera.lookAt(0,0,0);camera.updateMatrixWorld();
  const hit=pickTopology(camera,new THREE.Vector2(0.3,0),{width:400,height:400},'edge',[{instance_id:'sphere',matrix:new THREE.Matrix4(),geometry}]);
  assert.deepEqual(hit,{instance_id:'sphere',entity_id:'edge:0'});
  assert.equal(data.exact.curve_type,'CIRCLE');assert.equal(data.exact.radius,25);geometry.dispose();
});
