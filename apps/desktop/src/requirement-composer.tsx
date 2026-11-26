import React,{useEffect,useId,useLayoutEffect,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import type {RequirementModel} from './motion-tests';
import {parseTokens,tokenFor} from './requirement-tokens';
import {MentionList,clampIndex,filterMentions,matchesMention,mentionPopupPosition,orderMentions,useMentionSelection,type MentionChoice} from './mention-list';
import {PART_FORMAT_ORDER,parsePartFile,partFileKey} from './part-files';

type Props={
  id?:string;
  /** Plain string with `@[Label](entry#builder)` tokens. */
  value:string;
  onChange:(value:string)=>void;
  models:RequirementModel[];
  /** Published part names from the current built project, keyed to their model. */
  parts?:RequirementPart[];
  /** Models open in tabs; they list first until the user types a filter. */
  openModels?:RequirementModel['model'][];
  label:string;
  placeholder?:string;
  autoFocus?:boolean;
  /** Ctrl/Cmd+Enter. */
  onSubmit?:()=>void;
};
type Mention={node:Text;start:number;end:number;query:string};
export type RequirementPart={model:RequirementModel['model'];modelLabel:string;name:string;detail?:string};
type PickItem={kind:'model';item:RequirementModel}|{kind:'part';item:RequirementPart};
type ModelRow={model:RequirementModel;label?:string;detail?:string;members?:RequirementModel['model'][]};
const exactRank=(path:string)=>{const parsed=parsePartFile(path);return parsed?.extension==='stl'?1:0;};

/**
 * Fold STL and STEP model entries for one part into a single suggestion row.
 * The requirement still targets one model, preferring exact STEP over mesh STL.
 */
function groupModelRows(models:RequirementModel[]):ModelRow[]{
  const groups=new Map<string,{models:RequirementModel[];formats:string[]}>();
  for(const item of models){
    const parsed=parsePartFile(item.model.entry),key=partFileKey(item.model.entry);
    if(!parsed||!key)continue;
    const group=groups.get(key)??{models:[],formats:[]};
    if(!group.models.some(existing=>existing.model.entry===item.model.entry&&existing.model.builder===item.model.builder))group.models.push(item);
    if(!group.formats.includes(parsed.format))group.formats.push(parsed.format);
    groups.set(key,group);
  }
  const seen=new Set<string>(),rows:ModelRow[]=[];
  for(const item of models){
    const parsed=parsePartFile(item.model.entry),key=partFileKey(item.model.entry);
    if(!parsed||!key){rows.push({model:item});continue;}
    if(seen.has(key))continue;
    seen.add(key);
    const group=groups.get(key)!;
    if(group.models.length===1){rows.push({model:item});continue;}
    const preferred=[...group.models].sort((a,b)=>exactRank(a.model.entry)-exactRank(b.model.entry))[0];
    const preferredFile=parsePartFile(preferred.model.entry)!;
    const formats=[...group.formats].sort((a,b)=>PART_FORMAT_ORDER.indexOf(a)-PART_FORMAT_ORDER.indexOf(b));
    rows.push({model:preferred,label:`${preferredFile.directory}${preferredFile.stem}`,detail:formats.join(' + '),members:group.models.map(item=>item.model)});
  }
  return rows;
}

const keyOf=(entry:string,builder:string)=>JSON.stringify([entry,builder]);
const isChip=(node:Node|null|undefined):node is HTMLElement=>node?.nodeType===1&&(node as HTMLElement).classList.contains('req-chip');
const isPad=(node:Node)=>(node as HTMLElement).dataset?.pad==='1';

function makeChip(seg:{label:string;entry:string;builder:string;part?:string},models:RequirementModel[]){
  const chip=document.createElement('span');
  chip.className='req-chip';chip.setAttribute('contenteditable','false');
  chip.dataset.entry=seg.entry;chip.dataset.builder=seg.builder;
  if(seg.part)chip.dataset.part=seg.part;
  chip.dataset.label=seg.label;
  paintChip(chip,models);
  return chip;
}
/** Resolved chips show the model's current label; a model that left the project turns into a warning. */
function paintChip(chip:HTMLElement,models:RequirementModel[]){
  const found=models.find(item=>item.model.entry===chip.dataset.entry&&item.model.builder===chip.dataset.builder);
  const part=chip.dataset.part;
  if(found)chip.dataset.label=part?`${found.label} / ${part}`:found.label;
  const label=chip.dataset.label??'';
  chip.classList.toggle('unresolved',!found);
  chip.dataset.state=found?'ok':'missing';
  chip.title=found?(part?`Part ${part} in ${found.label}`:(found.detail??chip.dataset.entry??'')):`${label} is no longer in this project`;
  chip.setAttribute('aria-label',found?(part?`Part ${part} in ${found.label}`:`Model ${label}`):`Missing model ${label}`);
  // Inline chips cannot use CSS ellipsis, so keep very long labels from spilling
  // out of the editor; serialization still uses the full dataset label.
  const text=found?label:`${label} (missing)`;
  chip.textContent=text.length>48?`${text.slice(0,47)}…`:text;
}

/** Text becomes text nodes with a <br> per newline, so every line break is explicit. */
function textNodes(text:string):Node[]{
  const nodes:Node[]=[];
  text.split('\n').forEach((line,index)=>{
    if(index)nodes.push(document.createElement('br'));
    if(line)nodes.push(document.createTextNode(line));
  });
  return nodes;
}
const nodesFor=(text:string,models:RequirementModel[])=>parseTokens(text).flatMap(seg=>seg.kind==='text'?textNodes(seg.text):[makeChip(seg,models)]);

function serialize(root:Node):string{
  let out='';
  for(const child of Array.from(root.childNodes)){
    if(child.nodeType===Node.TEXT_NODE)out+=(child.textContent??'').replace(/ /g,' ');
    else if(isChip(child))out+=tokenFor({label:child.dataset.label??'',entry:child.dataset.entry??'',builder:child.dataset.builder??'',part:child.dataset.part});
    else if(child.nodeName==='BR'){if(!isPad(child))out+='\n';}
    else if(child.nodeType===1){
      // Browsers may wrap lines in blocks; flatten them to newlines.
      if(out&&!out.endsWith('\n'))out+='\n';
      out+=serialize(child);
    }
  }
  return out;
}

export function RequirementComposer({id,value,onChange,models,parts=[],openModels=[],label,placeholder,autoFocus,onSubmit}:Props){
  const base=useId().replace(/:/g,'');
  const inputId=id??`composer-${base}`,listId=`${inputId}-list`,hintId=`${inputId}-hint`;
  const editor=useRef<HTMLDivElement>(null);
  const lastValue=useRef<string|null>(null);
  const dismissed=useRef<{node:Node;start:number}|null>(null);
  const [mention,setMention]=useState<Mention|null>(null);
  const [empty,setEmpty]=useState(!value);
  const [position,setPosition]=useState<React.CSSProperties>({});

  const query=mention?.query??'';
  const openRank=(model:RequirementModel['model']) => {
    let best=Number.MAX_SAFE_INTEGER;
    openModels.forEach((open,index)=>{if(keyOf(open.entry,open.builder)===keyOf(model.entry,model.builder))best=Math.min(best,index);});
    return best;
  };
  const modelRows=groupModelRows(models);
  const modelChoice=(row:ModelRow):MentionChoice=>({key:`${keyOf(row.model.model.entry,row.model.model.builder)}:model`,label:row.label??row.model.label,detail:row.detail??row.model.detail});
  const partChoice=(item:RequirementPart):MentionChoice=>({key:`${keyOf(item.model.entry,item.model.builder)}:part:${item.name}`,label:`${item.modelLabel} / ${item.name}`,detail:`Part in ${item.modelLabel}${item.detail?` · ${item.detail}`:''}`});
  // Each row keeps its source item so the list can render labels while selection
  // still yields the model or part that produced it. Open tabs rank first until
  // the user types; then orderMentions puts the closest match on top.
  const sources:{choice:MentionChoice;item:PickItem}[]=mention?[
    ...modelRows.map(row=>({choice:{...modelChoice(row),rank:Math.min(...(row.members??[row.model.model]).map(openRank))},item:{kind:'model' as const,item:row.model}})),
    ...parts.map(item=>({choice:{...partChoice(item),rank:openRank(item.model)},item:{kind:'part' as const,item}})),
  ]:[];
  const filtered=filterMentions(sources.map(source=>source.choice),query);
  const choices=orderMentions(filtered,query);
  const {active,selected,move,hover,reset}=useMentionSelection(choices.length);
  const byKey=new Map(sources.map(source=>[source.choice.key,source.item]));
  const open=!!mention;
  const activeIndex=clampIndex(active,choices.length);
  const optionId=(index:number)=>`${inputId}-option-${index}`;
  const pick=(choice:MentionChoice)=>byKey.get(choice.key);
  function chooseChoice(choice:MentionChoice){const item=pick(choice);if(item)chooseMany([item]);}
  function chooseChoices(list:MentionChoice[]){chooseMany(list.map(pick).filter((item):item is PickItem=>!!item));}
  function mentionMatches(query:string){
    return modelRows.some(row=>matchesMention(modelChoice(row),query))||parts.some(item=>matchesMention(partChoice(item),query));
  }

  /** Keep a spare <br> after a trailing newline: browsers do not draw an empty last line otherwise. */
  function padTrailing(root:HTMLElement){
    root.querySelectorAll(':scope>br[data-pad]').forEach(node=>node.remove());
    if(root.lastChild?.nodeName==='BR'){const pad=document.createElement('br');pad.dataset.pad='1';root.append(pad);}
  }
  function fill(root:HTMLElement,text:string){
    root.replaceChildren(...nodesFor(text,models));padTrailing(root);
  }

  // The DOM is the source of truth while typing; an outside change (reset, seed) rebuilds it.
  useLayoutEffect(()=>{
    const root=editor.current;if(!root||value===lastValue.current)return;
    fill(root,value);lastValue.current=value;setEmpty(!value);
  },[value]);
  useEffect(()=>{
    const root=editor.current;if(!root)return;
    root.querySelectorAll<HTMLElement>('.req-chip').forEach(chip=>paintChip(chip,models));
    const next=serialize(root);
    if(next!==lastValue.current){lastValue.current=next;onChange(next);}
  },[models]);
  useEffect(()=>{if(autoFocus)editor.current?.focus();},[]);

  function place(){
    const root=editor.current;if(!root)return;
    setPosition(mentionPopupPosition(root.getBoundingClientRect(),{minWidth:420}));
  }
  useEffect(()=>{
    if(!open)return;
    place();
    const reposition=()=>place();
    window.addEventListener('resize',reposition);
    window.addEventListener('scroll',reposition,true);
    return()=>{window.removeEventListener('resize',reposition);window.removeEventListener('scroll',reposition,true);};
  },[open]);

  function emit(){
    const root=editor.current;if(!root)return;
    if(!root.querySelector('.req-chip,br:not([data-pad])')&&!(root.textContent??''))root.replaceChildren();
    else padTrailing(root);
    const next=serialize(root);
    setEmpty(!next);
    if(next!==lastValue.current){lastValue.current=next;onChange(next);}
  }

  function detect(){
    const root=editor.current,selection=window.getSelection();
    if(!root||!selection||!selection.rangeCount||!selection.isCollapsed||!root.contains(selection.anchorNode)){setMention(null);return;}
    const node=selection.anchorNode;
    if(!node||node.nodeType!==Node.TEXT_NODE){setMention(null);dismissed.current=null;return;}
    const offset=selection.anchorOffset;
    const text=(node.textContent??'').slice(0,offset);
    const match=/(^|[\s ])@([^@\[\]()\r\n]*)$/.exec(text);
    if(!match){setMention(null);dismissed.current=null;return;}
    const start=text.length-match[2].length-1,nextQuery=match[2],trimmed=nextQuery.trim();
    // Spaces are part of the query ("@v5 fra"), but a multi-word query that no
    // longer matches anything ends the mention instead of lingering forever.
    const closes=!trimmed?nextQuery.length>0:trimmed.includes(' ')&&!mentionMatches(trimmed);
    if(closes){setMention(null);dismissed.current=null;return;}
    if(dismissed.current&&dismissed.current.node===node&&dismissed.current.start===start){setMention(null);return;}
    setMention(current=>current&&current.node===node&&current.start===start&&current.end===offset?current:{node:node as Text,start,end:offset,query:nextQuery});
    reset();
  }
  useEffect(()=>{
    document.addEventListener('selectionchange',detect);
    return()=>document.removeEventListener('selectionchange',detect);
  },[]);

  /** Replace `range` with `nodes`, leaving the caret after them. */
  function insertAt(range:Range,nodes:Node[]){
    range.deleteContents();
    const fragment=document.createDocumentFragment();
    fragment.append(...nodes);
    const tail=fragment.lastChild;
    range.insertNode(fragment);
    if(tail){
      const after=document.createRange();after.setStartAfter(tail);after.collapse(true);
      const selection=window.getSelection();selection?.removeAllRanges();selection?.addRange(after);
    }
  }

  function chooseMany(picks:PickItem[]){
    const current=mention;if(!current||!picks.length)return;
    const range=document.createRange();
    range.setStart(current.node,current.start);range.setEnd(current.node,current.end);
    // The space after each chip gives the caret a text position to rest in.
    const nodes=picks.flatMap(choice=>{
      const target=choice.kind==='part'
        ?{label:`${choice.item.modelLabel} / ${choice.item.name}`,entry:choice.item.model.entry,builder:choice.item.model.builder,part:choice.item.name}
        :{label:choice.item.label,entry:choice.item.model.entry,builder:choice.item.model.builder};
      return [makeChip(target,models),document.createTextNode(' ')];
    });
    insertAt(range,nodes);
    setMention(null);dismissed.current=null;emit();
  }

  /** The chip directly before (or after) a collapsed caret, if any. */
  function adjacentChip(forward:boolean):HTMLElement|null{
    const selection=window.getSelection();
    if(!selection||!selection.rangeCount||!selection.isCollapsed)return null;
    const node=selection.anchorNode,offset=selection.anchorOffset;
    if(!node)return null;
    let neighbour:Node|null;
    if(node.nodeType===Node.TEXT_NODE){
      const length=(node.textContent??'').length;
      if(!forward&&offset===0)neighbour=node.previousSibling;
      else if(forward&&offset===length)neighbour=node.nextSibling;
      else return null;
    }else neighbour=forward?node.childNodes[offset]??null:node.childNodes[offset-1]??null;
    while(neighbour&&neighbour.nodeType===Node.TEXT_NODE&&!(neighbour.textContent??'')){
      neighbour=forward?neighbour.nextSibling:neighbour.previousSibling;
    }
    return isChip(neighbour)?neighbour:null;
  }

  function onKeyDown(event:React.KeyboardEvent<HTMLDivElement>){
    if(event.nativeEvent.isComposing)return;
    const submit=event.key==='Enter'&&(event.metaKey||event.ctrlKey);
    if(open&&choices.length&&!submit){
      if(event.key==='ArrowDown'){event.preventDefault();move(1,event.shiftKey);return;}
      if(event.key==='ArrowUp'){event.preventDefault();move(-1,event.shiftKey);return;}
      if((event.key==='Enter'||event.key==='Tab')&&activeIndex>=0){
        event.preventDefault();
        const picks=[...selected].sort((a,b)=>a-b).map(index=>choices[index]).filter((choice):choice is MentionChoice=>!!choice);
        if(picks.length)chooseChoices(picks);
        return;
      }
    }
    if(open&&event.key==='Escape'){
      event.preventDefault();event.stopPropagation();
      if(mention)dismissed.current={node:mention.node,start:mention.start};
      setMention(null);return;
    }
    if(event.key==='Enter'){
      event.preventDefault();
      if(submit){onSubmit?.();return;}
      const selection=window.getSelection();
      if(selection?.rangeCount)insertAt(selection.getRangeAt(0),[document.createElement('br')]);
      emit();return;
    }
    if((event.key==='Backspace'||event.key==='Delete')&&!event.metaKey&&!event.altKey&&!event.ctrlKey){
      const chip=adjacentChip(event.key==='Delete');
      if(chip){
        event.preventDefault();
        const range=document.createRange();range.setStartBefore(chip);range.collapse(true);
        chip.remove();
        const selection=window.getSelection();selection?.removeAllRanges();selection?.addRange(range);
        emit();detect();
      }
    }
  }

  function onPaste(event:React.ClipboardEvent<HTMLDivElement>){
    event.preventDefault();
    const text=event.clipboardData.getData('text/plain').replace(/\r\n?/g,'\n');
    const selection=window.getSelection();
    if(!text||!selection?.rangeCount)return;
    insertAt(selection.getRangeAt(0),nodesFor(text,models));emit();
  }
  // Copy a selection as tokens, so pasting elsewhere (or back) keeps the chips.
  function onCopy(event:React.ClipboardEvent<HTMLDivElement>){
    const selection=window.getSelection();
    if(!selection?.rangeCount||selection.isCollapsed)return;
    const holder=document.createElement('div');holder.append(selection.getRangeAt(0).cloneContents());
    event.clipboardData.setData('text/plain',serialize(holder));event.preventDefault();
  }
  function onCut(event:React.ClipboardEvent<HTMLDivElement>){
    onCopy(event);
    const selection=window.getSelection();
    if(selection?.rangeCount&&!selection.isCollapsed){selection.getRangeAt(0).deleteContents();emit();}
  }

  return <div className="req-composer">
    <label id={`${inputId}-label`} onClick={()=>editor.current?.focus()}>{label}</label>
    <div className="req-editor-wrap">
      <div ref={editor} id={inputId} className="req-editor" role="textbox" aria-multiline="true" aria-labelledby={`${inputId}-label`} aria-describedby={hintId}
        aria-haspopup="listbox" aria-expanded={open} aria-controls={open?listId:undefined} aria-autocomplete="list"
        aria-activedescendant={open&&choices.length&&activeIndex>=0?optionId(activeIndex):undefined}
        contentEditable suppressContentEditableWarning spellCheck data-empty={empty||undefined} data-placeholder={placeholder}
        tabIndex={0} onInput={()=>{emit();detect();}} onKeyDown={onKeyDown} onPaste={onPaste} onCopy={onCopy} onCut={onCut}
        onBlur={()=>{setMention(null);dismissed.current=null;}}/>
      {open&&createPortal(<MentionList id={listId} label="Models and parts in this project" choices={choices} activeIndex={activeIndex} optionId={optionId}
        className="req-picker" optionClass="req-option" activeClass="on" onMouseDown={event=>event.preventDefault()}
        style={position} selected={selected} onHover={hover} onChoose={chooseChoice}
        empty={<div className="req-picker-empty" role="status">{models.length||parts.length?'No matching model or part':'No models in this project'}</div>}/>,document.body)}
    </div>
    <p id={hintId} className="muted">Type @ to mention a model or its published part. Enter adds a line; Cmd/Ctrl+Enter prepares.</p>
  </div>;
}
