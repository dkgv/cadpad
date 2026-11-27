import test from 'node:test';
import assert from 'node:assert/strict';
import * as THREE from 'three';
import {diffSurfaces,type ReviewPart} from '../src/review-diff.ts';

function part(id:string,geometry:THREE.BufferGeometry,position:[number,number,number]=[0,0,0],volume:number|null=null):ReviewPart{
  const indexed=geometry.index?geometry:geometry.setIndex([...Array(geometry.attributes.position.count).keys()]);
  return {instance_id:id,name:id,position,volume,vertices:Float32Array.from(indexed.attributes.position.array),triangles:Uint32Array.from(indexed.index!.array)};
}
function bounds(points:Float32Array){
  const box=new THREE.Box3();
  for(let i=0;i<points.length;i+=3)box.expandByPoint(new THREE.Vector3(points[i],points[i+1],points[i+2]));
  return box;
}

test('identical revisions report no change without sampling',()=>{
  const box=part('a',new THREE.BoxGeometry(10,10,10),[0,0,0],1000);
  const diff=diffSurfaces([box],[{...box}]);
  assert.equal(diff.identical,true);
  assert.equal(diff.added.length,0);assert.equal(diff.removed.length,0);
  assert.equal(diff.volumeBefore,1000);assert.equal(diff.volumeAfter,1000);
});

test('a taller box adds side and top surface and removes only the old top',()=>{
  const before=part('a',new THREE.BoxGeometry(10,10,10).translate(0,0,5));
  const after=part('a',new THREE.BoxGeometry(10,10,16).translate(0,0,8));
  const diff=diffSurfaces([before],[after]);
  assert.equal(diff.identical,false);
  // Old top face (z=10) is gone; new material spans z in (10,16].
  const removed=bounds(diff.removed);
  assert.ok(Math.abs(removed.min.z-10)<1e-4&&Math.abs(removed.max.z-10)<1e-4,'only the old top face is removed');
  // A tolerance-wide band along the old top edge touches the walls and counts as unchanged.
  assert.ok(Math.abs(diff.removedArea-100)<6,`removed area ${diff.removedArea}`);
  const added=bounds(diff.added);
  assert.ok(added.min.z>=10-diff.tolerance*2&&Math.abs(added.max.z-16)<1e-4,'added surface sits above the old top');
  // New top (100) plus four 10×6 strips (240), sampled at the bisection resolution.
  assert.ok(Math.abs(diff.addedArea-340)<25,`added area ${diff.addedArea}`);
});

test('a large face reports only the region that changed',()=>{
  // Two big triangles cover the plate before; after, a raised boss sits in the middle.
  const plate=new THREE.PlaneGeometry(100,100);
  const boss=new THREE.BoxGeometry(10,10,5).translate(0,0,2.5);
  const diff=diffSurfaces([part('plate',plate)],[part('plate',plate.clone()),part('boss',boss)]);
  const added=bounds(diff.added);
  assert.ok(added.max.x<=5+1e-4&&added.min.x>=-5-1e-4,'added region stays on the boss');
  assert.equal(diff.removed.length,0,'the plate is shared verbatim');
});

test('removing a part reports its surface as removed',()=>{
  const plate=part('plate',new THREE.BoxGeometry(50,50,2),[0,0,0],5000);
  const pin=part('pin',new THREE.CylinderGeometry(2,2,10,24),[20,20,6],125);
  const diff=diffSurfaces([plate,pin],[plate]);
  assert.equal(diff.added.length,0);
  const removed=bounds(diff.removed);
  assert.ok(removed.min.x>=18-1e-3&&removed.max.x<=22+1e-3&&removed.min.y>=15-1e-3,'removed surface is the pin');
  assert.equal(diff.volumeBefore,5125);assert.equal(diff.volumeAfter,5000);
});

test('unknown volumes stay unknown',()=>{
  const diff=diffSurfaces([part('a',new THREE.BoxGeometry(1,1,1),[0,0,0],null)],[part('a',new THREE.BoxGeometry(2,1,1))]);
  assert.equal(diff.volumeBefore,null);assert.equal(diff.volumeAfter,null);
});
