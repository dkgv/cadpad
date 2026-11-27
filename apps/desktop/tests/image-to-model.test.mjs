import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {build} from 'esbuild';
import {JSDOM} from 'jsdom';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import React,{act} from 'react';

test('Cadpad Cloud tab includes a collapsible History section that is empty by default',async()=>{
 const directory=await mkdtemp(path.join(process.cwd(),'target/cloud-ui-'));
 const result=await build({stdin:{contents:"export {ImageToModel} from './image-to-model';",resolveDir:new URL('../src/',import.meta.url).pathname,loader:'tsx'},bundle:true,format:'esm',platform:'node',write:false,external:['react','react-dom']});
 const file=path.join(directory,'cloud.mjs');
 await writeFile(file,result.outputFiles[0].text);
 const dom=new JSDOM('<div id="root"></div>',{url:'https://cadpad.test'});
 for(const key of ['window','document','navigator','HTMLElement','Node','Event','MouseEvent'])Object.defineProperty(globalThis,key,{value:dom.window[key],configurable:true});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const {createRoot}=await import('react-dom/client');
 const {ImageToModel}=await import(pathToFileURL(file));
 const root=createRoot(document.getElementById('root'));
 const baseProps={signedIn:true,signingIn:false,image:null,guidance:'',generating:false,versions:[],staged:0,disabled:false,onSignIn:()=>{},onChoose:()=>{},onFile:()=>{},onClear:()=>{},onGuidance:()=>{},onGenerate:()=>{},onVersion:()=>{},onDismissVersions:()=>{}};
 try{
  await act(async()=>root.render(React.createElement(ImageToModel,baseProps)));
  const toggle=document.querySelector('.image-model-history-heading button');
  assert.ok(toggle,'history toggle button should exist');
  assert.equal(toggle.getAttribute('aria-expanded'),'false','history section starts collapsed');
  assert.equal(document.getElementById('image-model-history-body'),null,'history body is hidden when collapsed');

  await act(async()=>{toggle.click();});
  assert.equal(toggle.getAttribute('aria-expanded'),'true','history section expands on click');
  const body=document.getElementById('image-model-history-body');
  assert.ok(body,'history body renders when expanded');
  assert.match(body.textContent,/No generation history yet\./);

  const selected=[];
  const history=[{id:'job-1',filename:'bracket.png',created_at:'2026-11-27'}];
  await act(async()=>root.render(React.createElement(ImageToModel,{...baseProps,history,onSelectHistory:item=>selected.push(item)})));
  assert.equal(document.querySelector('.image-model-history-count')?.textContent,'1');
  const itemButton=document.querySelector('.image-model-history-item');
  assert.ok(itemButton,'history item renders when provided');
  await act(async()=>{itemButton.click();});
  assert.deepEqual(selected,history);
 }finally{
  await act(async()=>root.unmount());
  await rm(directory,{recursive:true,force:true});
 }
});
