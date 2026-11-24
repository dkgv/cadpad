import React,{useEffect,useId,useLayoutEffect,useRef,useState} from 'react';
import type {RequirementModel} from './motion-tests';
import {parseTokens,tokenFor} from './requirement-tokens';

type Props={
  id?:string;
  /** Plain string with `@[Label](entry#builder)` tokens. */
  value:string;
  onChange:(value:string)=>void;
  models:RequirementModel[];
  label:string;
  placeholder?:string;
  autoFocus?:boolean;
  /** Ctrl/Cmd+Enter. */
  onSubmit?:()=>void;
};
type Mention={node:Text;start:number;end:number;query:string};

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
  if(found)chip.dataset.label=found.label;
  const label=chip.dataset.label??'';
  chip.classList.toggle('unresolved',!found);
  chip.dataset.state=found?'ok':'missing';
  chip.title=found?(found.detail??chip.dataset.entry??''):`${label} is no longer in this project`;
  chip.setAttribute('aria-label',found?`Model ${label}`:`Missing model ${label}`);
  chip.textContent=found?label:`${label} (missing)`;
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

export function RequirementComposer({id,value,onChange,models,label,placeholder,autoFocus,onSubmit}:Props){
  const base=useId().replace(/:/g,'');
  const inputId=id??`composer-${base}`,listId=`${inputId}-list`,hintId=`${inputId}-hint`;
  const editor=useRef<HTMLDivElement>(null);
  const lastValue=useRef<string|null>(null);
  const dismissed=useRef<{node:Node;start:number}|null>(null);
  const [mention,setMention]=useState<Mention|null>(null);
  const [active,setActive]=useState(0);
  const [empty,setEmpty]=useState(!value);

  const query=mention?.query.toLowerCase()??'';
  const items=mention?models.filter(item=>!query||item.label.toLowerCase().includes(query)||(item.detail??'').toLowerCase().includes(query)):[];
  const open=!!mention;
  const activeIndex=Math.min(active,Math.max(items.length-1,0));
  const optionId=(index:number)=>`${inputId}-option-${index}`;

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
    const match=/(^|[\s ])@([^\s@\[\]()]*)$/.exec(text);
    if(!match){setMention(null);dismissed.current=null;return;}
    const start=text.length-match[2].length-1;
    if(dismissed.current&&dismissed.current.node===node&&dismissed.current.start===start){setMention(null);return;}
    setMention(current=>current&&current.node===node&&current.start===start&&current.end===offset?current:{node:node as Text,start,end:offset,query:match[2]});
    setActive(0);
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

  function choose(item:RequirementModel){
    const current=mention;if(!current)return;
    const range=document.createRange();
    range.setStart(current.node,current.start);range.setEnd(current.node,current.end);
    // The space after the chip gives the caret a text position to rest in.
    insertAt(range,[makeChip({label:item.label,entry:item.model.entry,builder:item.model.builder},models),document.createTextNode(' ')]);
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
    if(open&&items.length&&!submit){
      if(event.key==='ArrowDown'){event.preventDefault();setActive((activeIndex+1)%items.length);return;}
      if(event.key==='ArrowUp'){event.preventDefault();setActive((activeIndex-1+items.length)%items.length);return;}
      if(event.key==='Enter'||event.key==='Tab'){event.preventDefault();choose(items[activeIndex]);return;}
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
        aria-activedescendant={open&&items.length?optionId(activeIndex):undefined}
        contentEditable suppressContentEditableWarning spellCheck data-empty={empty||undefined} data-placeholder={placeholder}
        tabIndex={0} onInput={()=>{emit();detect();}} onKeyDown={onKeyDown} onPaste={onPaste} onCopy={onCopy} onCut={onCut}
        onBlur={()=>{setMention(null);dismissed.current=null;}}/>
      {open&&<div className="req-picker" id={listId} role="listbox" aria-label="Models in this project" onMouseDown={event=>event.preventDefault()}>
        {items.length?items.map((item,index)=><div key={keyOf(item.model.entry,item.model.builder)} id={optionId(index)} role="option" aria-selected={index===activeIndex} className={index===activeIndex?'req-option on':'req-option'} onMouseEnter={()=>setActive(index)} onClick={()=>choose(item)}>
          <span>{item.label}</span>{item.detail&&<small>{item.detail}</small>}
        </div>):<div className="req-picker-empty" role="status">{models.length?'No matching model':'No models in this project'}</div>}
      </div>}
    </div>
    <p id={hintId} className="muted">Type @ to mention a model. Enter adds a line; Cmd/Ctrl+Enter prepares.</p>
  </div>;
}
