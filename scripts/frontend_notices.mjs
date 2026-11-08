/** Production Rollup input ownership and publisher notices; no network or installs. */
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';

const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const inside = (root, file) => {const relative=path.relative(root,file);return relative===''||!relative.startsWith('..'+path.sep)&&relative!=='..'&&!path.isAbsolute(relative);};
const slash = value => value.split(path.sep).join('/');
const safe = value => value.replace(/[^a-zA-Z0-9._-]/g,'_');

function ownerOf(file) {
  let directory=path.dirname(file);
  while(true){
    const manifest=path.join(directory,'package.json');
    if(fs.existsSync(manifest))return {root:directory,manifest,metadata:JSON.parse(fs.readFileSync(manifest,'utf8'))};
    const parent=path.dirname(directory);if(parent===directory)return null;directory=parent;
  }
}

function noticeFiles(root) {
  const result=[];
  function walk(directory, licenseDirectory=false, depth=0){
    if(depth>20)throw new Error('Notice directory nesting exceeds limit');
    for(const entry of fs.readdirSync(directory,{withFileTypes:true}).sort((a,b)=>a.name.localeCompare(b.name))){
      if(entry.name==='node_modules'||entry.name==='.git')continue;
      const file=path.join(directory,entry.name);
      if(entry.isDirectory())walk(file,licenseDirectory||/^licen[cs]es$/i.test(entry.name),depth+1);
      else if(licenseDirectory||/^(licen[cs]e|copying|copyright|notice)([._-].*|$)/i.test(entry.name)){
        const resolved=fs.realpathSync(file);
        if(!inside(root,resolved))throw new Error('Publisher notice symlink escapes package');
        if(fs.statSync(resolved).isFile())result.push({file:resolved,relative:slash(path.relative(root,file))});
      }
    }
  }
  walk(root);return result;
}

export function collectFrontend(bundle, repository, virtualSources=new Map()) {
  repository=fs.realpathSync(repository);
  const lockBytes=fs.readFileSync(path.join(repository,'package-lock.json'));
  const lock=JSON.parse(lockBytes);
  const workspaces=new Set([repository,...Object.values(lock.packages).filter(p=>p.link).map(p=>fs.realpathSync(path.resolve(repository,p.resolved)))]);
  const components=new Map(), material=new Map(), review=[], chunks=[], virtual=[], firstParty=[];
  function componentFor(owner){
    if(components.has(owner.root))return components.get(owner.root);
    if(!inside(repository,owner.root))throw new Error('Third-party package root is outside repository');
    const metadataBytes=fs.readFileSync(owner.manifest), metadataHash=sha(metadataBytes);
    const {name,version,license,repository:publisherRepository}=owner.metadata;
    if(typeof name!=='string'||typeof version!=='string')throw new Error('Missing package identity');
    const relative=slash(path.relative(repository,owner.root)),locked=lock.packages[relative];
    const id=`${safe(name)}-${safe(version)}-${sha(relative).slice(0,8)}-${metadataHash.slice(0,12)}`;
    const component={id,name,version,declared_license:license??null,publisher_repository:publisherRepository??null,
      installed_root:relative,package_json_sha256:metadataHash,locked_package:locked??null,
      installed_archive_integrity_verified:false,modules:[],notices:[],embedded_sources:[]};
    components.set(owner.root,component);
    if(!locked||locked.version!==version)review.push({component:id,issue:'installed-package-not-in-lock-at-version'});
    for(const item of noticeFiles(owner.root)){
      const bytes=fs.readFileSync(item.file),hash=sha(bytes);
      const file=`third-party/frontend/${id}/${hash}-${safe(path.basename(item.relative))}`;
      material.set(file,bytes);component.notices.push({source:item.relative,file,sha256:hash});
    }
    if(!component.notices.length)review.push({component:id,issue:'publisher-notice-missing'});
    return component;
  }
  function prebundledSources(owner, component, file){
    // Read only an adjacent publisher map explicitly named by this module.
    const content=fs.readFileSync(file,'utf8');
    const references=[...content.matchAll(/[#@]\s*sourceMappingURL=([^\s*]+)/g)];
    if(!references.length)return;
    const reference=references.at(-1)[1];
    if(reference.startsWith('data:')){review.push({component:component.id,issue:'inline-publisher-map-not-inspected'});return;}
    const mapFile=path.resolve(path.dirname(file),reference);
    if(!inside(owner.root,mapFile)||!fs.existsSync(mapFile)){
      review.push({component:component.id,issue:'publisher-source-map-unavailable',module:slash(path.relative(owner.root,file))});return;
    }
    if(!inside(owner.root,fs.realpathSync(mapFile)))throw new Error('Publisher map symlink escapes package');
    const bytes=fs.readFileSync(mapFile),map=JSON.parse(bytes);
    if(!Array.isArray(map.sources))throw new Error('Invalid publisher source map');
    const candidates=new Map();
    for(let index=0;index<map.sources.length;index++){
      const original=map.sources[index],marker='node_modules/',at=original.lastIndexOf(marker);
      if(at<0)continue;
      const pieces=original.slice(at+marker.length).split('/');
      const name=pieces[0].startsWith('@')?pieces.splice(0,2).join('/'):pieces.shift();
      const record={publisher_map_source:original,source_content_sha256:typeof map.sourcesContent?.[index]==='string'?sha(map.sourcesContent[index]):null,
        candidate_source_matches:false};
      if(!candidates.has(name))candidates.set(name,{name,sources:[]});
      const candidate=candidates.get(name);candidate.sources.push(record);
      let dependency;
      try{
        const resolved=createRequire(owner.manifest).resolve(name);
        dependency=ownerOf(fs.realpathSync(resolved));
        if(!dependency||dependency.metadata.name!==name)throw new Error('Cannot identify embedded dependency candidate');
      }catch(error){record.candidate_resolution_error=String(error.message);continue;}
      const matching=componentFor(dependency);candidate.candidate_component=matching.id;
      const dependencyFile=path.resolve(dependency.root,...pieces);
      if(record.source_content_sha256&&inside(dependency.root,dependencyFile)&&fs.existsSync(dependencyFile)&&inside(dependency.root,fs.realpathSync(dependencyFile))){
        record.candidate_source_matches=sha(fs.readFileSync(dependencyFile))===record.source_content_sha256;
      }
    }
    if(candidates.size){
      component.embedded_sources.push({module:slash(path.relative(owner.root,file)),publisher_map_sha256:sha(bytes),dependencies:[...candidates.values()]});
      review.push({component:component.id,issue:'publisher-prebundle-provenance-review-required'});
    }
  }
  const inspected=new Set();
  for(const output of Object.values(bundle).sort((a,b)=>a.fileName.localeCompare(b.fileName))){
    if(output.type!=='chunk')continue;
    chunks.push({file:output.fileName,sha256:sha(output.code)});
    for(const [id,info]of Object.entries(output.modules).sort(([a],[b])=>a.localeCompare(b))){
      if((!info.renderedLength&&!/\.css(?:\?|$)/.test(id))||inspected.has(id))continue;
      inspected.add(id);
      if(id.startsWith('\0')||!path.isAbsolute(id)){
        const label=id.replaceAll(repository,'<repository>'),source=virtualSources.get(id);
        virtual.push({id:label,source_sha256:typeof source==='string'?sha(source):null});
        if(id.startsWith('\0vite/')){
          const resolved=createRequire(path.join(repository,'apps/desktop/package.json')).resolve('vite');
          const owner=ownerOf(fs.realpathSync(resolved));
          if(!owner||owner.metadata.name!=='vite')throw new Error('Cannot identify Vite runtime helper');
          componentFor(owner).modules.push({virtual_id:label,source_sha256:typeof source==='string'?sha(source):null,rendered_length:info.renderedLength});
        }
        continue;
      }
      const filename=id.split('?')[0],file=fs.realpathSync(filename),owner=ownerOf(file);
      if(!owner){review.push({issue:'module-owner-missing',module:slash(path.relative(repository,file))});continue;}
      if(workspaces.has(owner.root)){firstParty.push(slash(path.relative(repository,file)));continue;}
      const component=componentFor(owner);
      component.modules.push({path:slash(path.relative(owner.root,file)),source_sha256:sha(fs.readFileSync(file)),rendered_length:info.renderedLength});
      prebundledSources(owner,component,file);
    }
  }
  const inventory={format:'cadpad-frontend-notices-v1',legal_clearance:false,scope:'Rendered production Rollup modules and publisher-supplied notices; prebundles require separate provenance review',
    package_lock_sha256:sha(lockBytes),chunks,components:[...components.values()].sort((a,b)=>a.id.localeCompare(b.id)),
    first_party_modules:firstParty.sort(),virtual_modules:virtual.sort((a,b)=>a.id.localeCompare(b.id)),review_required:review,
    release_gates:['Exact installed npm archive verification','Publisher prebundle and embedded dependency provenance','Rust and worker notices collected separately']};
  material.set('third-party/frontend/inventory.json',Buffer.from(JSON.stringify(inventory,null,2)+'\n'));
  return {inventory,material};
}

export function frontendNotices(repository=fileURLToPath(new URL('..',import.meta.url))){
  return {name:'cadpad-frontend-notices',apply:'build',enforce:'post',
    generateBundle:{order:'post',handler(_options,bundle){
      const virtualSources=new Map([...this.getModuleIds()].filter(id=>id.startsWith('\0')).map(id=>[id,this.getModuleInfo(id)?.code]));
      const {material}=collectFrontend(bundle,repository,virtualSources);
      for(const [fileName,source]of material)this.emitFile({type:'asset',fileName,source});
    }}};
}
