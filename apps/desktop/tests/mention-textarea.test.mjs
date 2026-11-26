// CPU tests for the Assistant file/folder mention rows; no browser or native app.
import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {build} from 'esbuild';
import path from 'node:path';
import {pathToFileURL} from 'node:url';

async function load(entry){
 await mkdir(path.join(process.cwd(),'target'),{recursive:true});
 const directory=await mkdtemp(path.join(process.cwd(),'target/mention-unit-'));
 const result=await build({entryPoints:[new URL(entry,import.meta.url).pathname],bundle:true,format:'esm',platform:'node',write:false,external:['react','react-dom']});
 const file=path.join(directory,'module.mjs');await writeFile(file,result.outputFiles[0].text);
 return {module:await import(pathToFileURL(file)),cleanup:()=>rm(directory,{recursive:true,force:true})};
}

test('project mention files group STL and STEP exports of one part',async()=>{
 const {module:{projectFileMentions},cleanup}=await load('../src/mention-textarea.tsx');
 try{
  const items=projectFileMentions(['parts/rail.stl','parts/rail.step','parts/rail.py','parts/carriage.stl']);
  // One row for the rail part, carrying both export paths.
  const rail=items.find(item=>item.label==='parts/rail');
  assert.ok(rail,'STL and STEP of the same part collapse into one row');
  assert.deepEqual(rail.paths,['parts/rail.stl','parts/rail.step']);
  assert.equal(rail.path,'parts/rail.stl');
  assert.equal(rail.detail,'STL + STEP');
  // A lone STL stays a normal path row.
  const carriage=items.find(item=>item.path==='parts/carriage.stl');
  assert.equal(carriage.paths,undefined);
  assert.equal(carriage.label,undefined);
  // Source files are untouched and the grouped part replaces both export rows.
  assert.ok(items.some(item=>item.path==='parts/rail.py'&&item.paths===undefined));
  assert.equal(items.filter(item=>item.path.startsWith('parts/rail.')).length,2);
  assert.equal(items.some(item=>item.path==='parts/rail.step'),false);
 }finally{await cleanup();}
});
