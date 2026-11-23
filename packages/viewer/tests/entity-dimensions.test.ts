import test from 'node:test';
import assert from 'node:assert/strict';
import {PickGeometry} from '../src/picking.ts';
import {entityDimensions} from '../src/entity-dimensions.ts';

// Two faces of a 10x20x30 box: top (z=30) and front (y=0), plus two edges.
const vertices=new Float32Array([0,0,30, 10,0,30, 10,20,30, 0,20,30, 0,0,0, 10,0,0]);
const triangles=new Uint32Array([0,1,2, 0,2,3, 4,5,1, 4,1,0]);
const ring:number[]=[];const n=32;
for(let i=0;i<n;i++){const a=i/n*Math.PI*2,b=(i+1)/n*Math.PI*2;ring.push(5+3*Math.cos(a),10+3*Math.sin(a),30,5+3*Math.cos(b),10+3*Math.sin(b),30);}
const edges=new Float32Array([0,0,30,10,0,30, ...ring]);
const geometry=new PickGeometry({vertices,triangles,edges,triangles_per_face:new Uint32Array([2,2]),segments_per_edge:new Uint32Array([1,n])});
const values=(id:string)=>entityDimensions('p',id,geometry).map(d=>[d.label,Math.round(d.value*1000)/1000]);

test('face dimensions measure the picked face, not the part',()=>{
  assert.deepEqual(values('face:0'),[['Face width',10],['Face depth',20]]);
  assert.deepEqual(values('face:1'),[['Face width',10],['Face height',30]]);
});
test('straight edge reports length; circular edge reports diameter',()=>{
  assert.deepEqual(values('edge:0'),[['Edge length',10]]);
  const [circle]=entityDimensions('p','edge:1',geometry);
  assert.equal(circle.kind,'diameter');assert.ok(Math.abs(circle.value-6)<1e-3);
});
test('part selections produce no entity dimensions',()=>{assert.deepEqual(values('part:0'),[]);});
test('planar face dimensions lift along the outward face normal',()=>{
  for(const d of entityDimensions('p','face:0',geometry))assert.deepEqual(d.offset?.map(x=>x+0),[0,0,1]);
  for(const d of entityDimensions('p','face:1',geometry))assert.deepEqual(d.offset?.map(x=>x+0),[0,-1,0]);
});
