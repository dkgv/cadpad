import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {collectFrontend} from '../frontend_notices.mjs';

function fixture(t){
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'cadpad-frontend-notices-'));
  t.after(()=>fs.rmSync(root,{recursive:true,force:true}));
  fs.writeFileSync(path.join(root,'package.json'),JSON.stringify({name:'first-party',version:'1.0.0',private:true}));
  const packages={};
  function dependency(name,{license='MIT',source='export const value=42;',version='1.0.0',identity=name}={}){
    const dir=path.join(root,'node_modules',name);fs.mkdirSync(dir,{recursive:true});
    fs.writeFileSync(path.join(dir,'package.json'),JSON.stringify({name:identity,version,license,main:'index.js'}));
    fs.writeFileSync(path.join(dir,'index.js'),source);
    fs.writeFileSync(path.join(dir,'LICENSE'),`${name} license\r\n`);
    packages['node_modules/'+name]={version,integrity:'sha512-candidate'};
    return dir;
  }
  function build(modules,virtualSources){
    fs.writeFileSync(path.join(root,'package-lock.json'),JSON.stringify({lockfileVersion:3,packages}));
    return collectFrontend({'assets/main.js':{type:'chunk',fileName:'assets/main.js',code:'production output',modules}},root,virtualSources);
  }
  return {root,dependency,build};
}

test('production module scope includes selected notices and excludes unused development packages',t=>{
  const {dependency,build}=fixture(t),selected=dependency('selected'),unused=dependency('unused');
  const {inventory,material}=build({[path.join(selected,'index.js')]:{renderedLength:30},[path.join(unused,'index.js')]:{renderedLength:0}});
  assert.deepEqual(inventory.components.map(c=>c.name),['selected']);
  const notice=inventory.components[0].notices[0];
  assert.equal(material.get(notice.file).toString(),'selected license\r\n');
  assert.equal(createHash('sha256').update(material.get(notice.file)).digest('hex'),notice.sha256);
  assert.equal(inventory.legal_clearance,false);
  assert.equal(inventory.components[0].installed_archive_integrity_verified,false);
});

test('publisher prebundle retains candidate notices and reports exact source comparisons',t=>{
  const {dependency,build}=fixture(t),embedded=dependency('embedded',{license:'ISC'});
  const wrapper=dependency('wrapper',{source:'export const value=42;\n//# sourceMappingURL=index.js.map\n'});
  fs.writeFileSync(path.join(wrapper,'index.js.map'),JSON.stringify({version:3,sources:['../node_modules/embedded/index.js'],sourcesContent:[fs.readFileSync(path.join(embedded,'index.js'),'utf8')]}));
  let {inventory}=build({[path.join(wrapper,'index.js')]:{renderedLength:30}});
  const source=inventory.components.find(c=>c.name==='wrapper').embedded_sources[0].dependencies[0].sources[0];
  assert.equal(source.candidate_source_matches,true);
  assert.equal(inventory.components.find(c=>c.name==='embedded').declared_license,'ISC');
  assert.ok(inventory.review_required.some(item=>item.issue==='publisher-prebundle-provenance-review-required'));
  fs.writeFileSync(path.join(embedded,'index.js'),'changed source');
  ({inventory}=build({[path.join(wrapper,'index.js')]:{renderedLength:30}}));
  assert.equal(inventory.components.find(c=>c.name==='wrapper').embedded_sources[0].dependencies[0].sources[0].candidate_source_matches,false);
});

test('publisher notice symlink cannot copy files outside its package',t=>{
  const {root,dependency,build}=fixture(t),selected=dependency('selected');
  fs.writeFileSync(path.join(root,'private'),'private material');
  fs.unlinkSync(path.join(selected,'LICENSE'));fs.symlinkSync(path.join(root,'private'),path.join(selected,'LICENSE'));
  assert.throws(()=>build({[path.join(selected,'index.js')]:{renderedLength:1}}),/symlink escapes/);
});

test('publisher source map outside its package remains an explicit review item',t=>{
  const {dependency,build}=fixture(t),selected=dependency('selected',{source:'export const value=42;\n//# sourceMappingURL=../../private.map'});
  const {inventory}=build({[path.join(selected,'index.js')]:{renderedLength:1}});
  assert.ok(inventory.review_required.some(item=>item.issue==='publisher-source-map-unavailable'));
});

test('two installed copies retain distinct identities and destinations',t=>{
  const {dependency,build}=fixture(t),first=dependency('shared'),second=dependency('parent/node_modules/shared',{identity:'shared'});
  const {inventory,material}=build({[path.join(first,'index.js')]:{renderedLength:1},[path.join(second,'index.js')]:{renderedLength:1}});
  assert.equal(inventory.components.length,2);
  assert.equal(new Set(inventory.components.map(c=>c.id)).size,2);
  assert.ok(inventory.components.every(c=>material.has(c.notices[0].file)));
});

test('CSS-only module retains its publisher notice',t=>{
  const {dependency,build}=fixture(t),selected=dependency('styles');fs.writeFileSync(path.join(selected,'theme.css'),'body{color:red}');
  const {inventory}=build({[path.join(selected,'theme.css')]:{renderedLength:0}});
  assert.equal(inventory.components[0].name,'styles');
});

test('injected Vite runtime helper retains its own publisher notice',t=>{
  const {dependency,build}=fixture(t);dependency('vite');
  const {inventory}=build({'\0vite/modulepreload-polyfill.js':{renderedLength:10}},new Map([['\0vite/modulepreload-polyfill.js','runtime helper']]));
  assert.equal(inventory.components[0].name,'vite');
  assert.ok(inventory.components[0].modules[0].source_sha256);
});

test('prebundle dependency notice cannot escape via a symlink',t=>{
  const {root,dependency,build}=fixture(t),embedded=dependency('embedded');
  const wrapper=dependency('wrapper',{source:'export const value=42;\n//# sourceMappingURL=index.js.map\n'});
  fs.writeFileSync(path.join(wrapper,'index.js.map'),JSON.stringify({version:3,sources:['../node_modules/embedded/index.js'],sourcesContent:['source']}));
  fs.writeFileSync(path.join(root,'private'),'private material');
  fs.unlinkSync(path.join(embedded,'LICENSE'));fs.symlinkSync(path.join(root,'private'),path.join(embedded,'LICENSE'));
  assert.throws(()=>build({[path.join(wrapper,'index.js')]:{renderedLength:1}}),/symlink escapes/);
});
