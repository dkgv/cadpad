import test from 'node:test';
import assert from 'node:assert/strict';
import * as THREE from 'three';
import {framedZoom,projectedHalfExtent} from '../src/fit-zoom.ts';

test('fit zoom fills a partially framed box and leaves a slim margin',()=>{
  const camera=new THREE.OrthographicCamera(-10,10,10,-10,0.1,100);
  camera.position.set(0,0,20);camera.lookAt(0,0,0);camera.updateProjectionMatrix();
  const box=new THREE.Box3(new THREE.Vector3(-5,-5,-5),new THREE.Vector3(5,5,5));
  const span=projectedHalfExtent(camera,box);
  assert.ok(Math.abs(span-0.5)<1e-6);
  assert.ok(Math.abs(framedZoom(1,span)-1.84)<1e-6);
});

test('fit zoom backs off when the box already touches the frame',()=>{
  assert.ok(Math.abs(framedZoom(1,1)-0.92)<1e-6);
});

test('fit zoom ignores a missing or non-finite extent',()=>{
  assert.equal(framedZoom(1.5,0),1.5);
  assert.equal(framedZoom(1.5,Number.NaN),1.5);
  assert.equal(framedZoom(0,0.4),0);
});
