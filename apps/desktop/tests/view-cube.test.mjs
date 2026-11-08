import assert from 'node:assert/strict';
import test from 'node:test';
import {dirname,join} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {spawnSync} from 'node:child_process';

function matrices(){
  const file=join(dirname(fileURLToPath(import.meta.url)),'../src/view-cube-math.ts');
  const result=spawnSync(process.execPath,['--experimental-strip-types','--input-type=module','-e',`
    import {cameraToCssMatrix} from ${JSON.stringify(pathToFileURL(file).href)};
    const parse=value=>value.slice('matrix3d('.length,-1).split(',').map(Number);
    const identity=cameraToCssMatrix(0,0,0,1);
    const yaw=cameraToCssMatrix(0,0.70710678118,0,0.70710678118);
    console.log(JSON.stringify({identity:parse(identity),yaw:parse(yaw)}));
  `],{encoding:'utf8'});
  if(result.status!==0)throw new Error(result.stderr||result.stdout||'failed to load view-cube-math');
  return JSON.parse(result.stdout.trim());
}

test('identity camera quaternion maps to a unit CSS matrix',()=>{
  const {identity}=matrices();
  assert.equal(identity.length,16);
  assert.ok(Math.abs(identity[0]-1)<1e-9);
  assert.ok(Math.abs(identity[5]-1)<1e-9);
  assert.ok(Math.abs(identity[10]-1)<1e-9);
  assert.equal(identity[15],1);
  for(const index of [1,2,3,4,6,7,8,9,11,12,13,14])assert.ok(Math.abs(identity[index])<1e-9);
});

test('yaw rotation changes the CSS matrix off the identity',()=>{
  const {identity,yaw}=matrices();
  assert.notDeepEqual(identity,yaw);
  assert.equal(yaw.length,16);
  assert.equal(yaw[15],1);
});
