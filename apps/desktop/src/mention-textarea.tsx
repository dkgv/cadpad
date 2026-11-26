import React,{useEffect,useId,useRef,useState} from 'react';
import {createPortal} from 'react-dom';
import {Icon} from './ui';
import {MentionList,clampIndex,filterMentions,matchesMention,mentionPopupPosition,orderMentions,useMentionSelection,type MentionChoice} from './mention-list';
import {PART_FORMAT_ORDER,parsePartFile,partFileKey,partFileRank} from './part-files';

const byName=new Intl.Collator(undefined,{sensitivity:'base'}).compare;

function normalize(path:string){
  return path.replaceAll('\\','/').replace(/^\.\//,'').replace(/\/+$/,'');
}

/** Distinct project-relative files, sorted for the mention list. */
export function projectFiles(files:string[]=[]):string[]{
  return [...new Set(files.map(normalize).filter(path=>path&&path!=='.'&&path!=='..'))].sort(byName);
}

/** Distinct project-relative folders that contain indexed files, parents before children. */
export function projectFolders(files:string[]=[]):string[]{
  const folders=new Set<string>();
  for(const path of projectFiles(files)){
    const parts=path.split('/');
    if(parts.length<2)continue;
    parts.pop();
    let prefix='';
    for(const part of parts){
      if(!part||part==='.'||part==='..')continue;
      prefix=prefix?`${prefix}/${part}`:part;
      folders.add(prefix);
    }
  }
  return [...folders].sort(byName);
}

export type MentionItem={kind:'folder'|'file';path:string;label?:string;detail?:string;paths?:string[]};

/**
 * File rows for the mention list. `part.stl` and `part.step` are two exports of
 * the same part, so they become one row (detail "STL + STEP") that inserts both
 * paths when picked.
 */
export function projectFileMentions(files:string[]=[]):MentionItem[]{
  const groups=new Map<string,{files:string[];formats:string[]}>();
  const items:MentionItem[]=[];
  for(const path of projectFiles(files)){
    const parsed=parsePartFile(path);
    const key=partFileKey(path);
    if(!parsed||!key){items.push({kind:'file',path});continue;}
    const group=groups.get(key)??{files:[],formats:[]};
    if(!group.files.includes(path))group.files.push(path);
    if(!group.formats.includes(parsed.format))group.formats.push(parsed.format);
    groups.set(key,group);
  }
  for(const group of groups.values()){
    const ordered=[...group.files].sort((a,b)=>partFileRank(a)-partFileRank(b)||byName(a,b));
    if(ordered.length===1){items.push({kind:'file',path:ordered[0]});continue;}
    const preferred=parsePartFile(ordered[0])!;
    const formats=[...group.formats].sort((a,b)=>PART_FORMAT_ORDER.indexOf(a)-PART_FORMAT_ORDER.indexOf(b));
    items.push({kind:'file',path:ordered[0],paths:ordered,label:`${preferred.directory}${preferred.stem}`,detail:formats.join(' + ')});
  }
  return items.sort((a,b)=>byName(a.label??a.path,b.label??b.path));
}

type Props=Omit<React.TextareaHTMLAttributes<HTMLTextAreaElement>,'value'|'onChange'> & {
  value:string;
  onValueChange:(value:string)=>void;
  items:MentionItem[];
  onMention?:(item:MentionItem)=>void;
  /** Project-relative entries open in model tabs; they list first until a filter is typed. */
  openModels?:string[];
};

type Mention={start:number;end:number;query:string};

/** A textarea that opens a file/folder picker when the user types `@`. */
export function MentionTextarea({value,onValueChange,items,onMention,openModels=[],onKeyDown,onPaste,onBlur,onFocus,onClick,...props}:Props){
  const [mention,setMention]=useState<Mention|null>(null);
  const [position,setPosition]=useState<React.CSSProperties>({left:0,top:0,width:320,maxHeight:300});
  const area=useRef<HTMLTextAreaElement>(null),popup=useRef<HTMLDivElement>(null),dismissed=useRef<{start:number}|null>(null),choosing=useRef(false);
  const id=useId();
  const open=!!mention;
  const query=mention?.query??'';
  // Files of open tabs (and their folders) come first until the user narrows the
  // list; then the closest match ranks on top.
  const openRank=(item:MentionItem)=>{
    let best=Number.MAX_SAFE_INTEGER;
    for(const path of item.paths??[item.path])openModels.forEach((entry,index)=>{if(entry===path||entry.startsWith(`${path}/`))best=Math.min(best,index);});
    return best;
  };
  const baseChoice=(item:MentionItem):MentionChoice=>({key:`${item.kind}:${item.path}`,label:item.label??item.path,detail:item.detail??item.kind,title:(item.paths??[item.path]).join(' · '),search:[...new Set([item.path,...(item.paths??[])])].join(' ')});
  // Only build rows while the picker is open; a large project index stays untouched otherwise.
  const sources=open?items.map(item=>({choice:{...baseChoice(item),rank:openRank(item),icon:<Icon name={item.kind==='folder'?'folder':'file'} size={14}/>},item})):[];
  const filtered=filterMentions(sources.map(source=>source.choice),query);
  const choices=orderMentions(filtered,query).slice(0,50);
  const {active,selected,move,hover,reset}=useMentionSelection(choices.length);
  const byKey=new Map(sources.map(source=>[source.choice.key,source.item]));
  const activeIndex=clampIndex(active,choices.length);
  const pick=(choice:MentionChoice)=>byKey.get(choice.key);
  function chooseChoice(choice:MentionChoice){const item=pick(choice);if(item)chooseMany([item]);}
  function chooseChoices(list:MentionChoice[]){chooseMany(list.map(pick).filter((item):item is MentionItem=>!!item));}
  function mentionMatches(query:string){return items.some(item=>matchesMention(baseChoice(item),query));}

  function close(){setMention(null);}
  function place(){
    const node=area.current;if(!node)return;
    setPosition(mentionPopupPosition(node.getBoundingClientRect(),{minWidth:420}));
  }
  function detect(node=area.current){
    if(choosing.current){close();return;}
    if(!node){close();return;}
    const caret=node.selectionStart??node.value.length;
    const before=node.value.slice(0,caret);
    const match=/(^|[\s\n])@([^@\r\n]*)$/.exec(before);
    if(!match){close();dismissed.current=null;return;}
    const start=before.length-match[2].length-1,nextQuery=match[2],trimmed=nextQuery.trim();
    // Spaces are part of the query ("@v5 fra"); close once a multi-word query
    // stops matching so the popup cannot linger through the rest of a sentence.
    const closes=!trimmed?nextQuery.length>0:trimmed.includes(' ')&&!mentionMatches(trimmed);
    if(closes){close();dismissed.current=null;return;}
    if(dismissed.current&&dismissed.current.start===start){close();return;}
    const same=!!mention&&mention.start===start&&mention.end===caret&&mention.query===nextQuery;
    setMention(current=>current&&current.start===start&&current.end===caret&&current.query===nextQuery?current:{start,end:caret,query:nextQuery});
    if(!same)reset();
    place();
  }

  useEffect(()=>{
    if(!open)return;
    const outside=(event:PointerEvent)=>{if(!popup.current?.contains(event.target as Node)&&!area.current?.contains(event.target as Node))close();};
    const dismiss=()=>close();
    document.addEventListener('pointerdown',outside,true);window.addEventListener('resize',dismiss);window.addEventListener('scroll',dismiss,true);
    return()=>{document.removeEventListener('pointerdown',outside,true);window.removeEventListener('resize',dismiss);window.removeEventListener('scroll',dismiss,true);};
  },[open]);
  useEffect(()=>{if(open&&!value)close();},[value,open]);
  useEffect(()=>{if(open)popup.current?.querySelector(`[data-mention-index="${activeIndex}"]`)?.scrollIntoView({block:'nearest'});},[activeIndex,open,query]);

  function chooseMany(picks:MentionItem[]){
    const current=mention,node=area.current;if(!current||!node||!picks.length)return;
    const parts=picks.flatMap(item=>(item.paths??[item.path]).map(path=>`@${path} `));
    const inserted=parts.join('');
    const next=node.value.slice(0,current.start)+inserted+node.value.slice(current.end);
    // Detection matches the last @ in the inserted run, so dismiss that one: the
    // trailing spaces (single or multi insert) must not reopen the picker.
    const last=parts[parts.length-1];
    choosing.current=true;onValueChange(next);picks.forEach(item=>onMention?.(item));close();
    dismissed.current={start:current.start+inserted.length-last.length};
    requestAnimationFrame(()=>{
      const caret=current.start+inserted.length;
      node.focus();node.setSelectionRange(caret,caret);choosing.current=false;detect(node);
    });
  }

  return <>
    <textarea {...props} ref={area} value={value} aria-haspopup="listbox" aria-expanded={open} aria-controls={open?`${id}-results`:undefined} aria-activedescendant={open&&choices.length&&activeIndex>=0?`${id}-option-${activeIndex}`:undefined}
      onChange={event=>{onValueChange(event.target.value);detect(event.currentTarget);}}
      onKeyDown={event=>{
        if(event.nativeEvent.isComposing){onKeyDown?.(event);return;}
        if(open&&choices.length){
          if(event.key==='ArrowDown'){event.preventDefault();event.stopPropagation();move(1,event.shiftKey);return;}
          if(event.key==='ArrowUp'){event.preventDefault();event.stopPropagation();move(-1,event.shiftKey);return;}
          if((event.key==='Enter'||event.key==='Tab')&&!event.shiftKey&&activeIndex>=0){
            event.preventDefault();event.stopPropagation();
            const picks=[...selected].sort((a,b)=>a-b).map(index=>choices[index]).filter((choice):choice is MentionChoice=>!!choice);
            if(picks.length)chooseChoices(picks);
            return;
          }
        }
        if(open&&event.key==='Escape'){event.preventDefault();event.stopPropagation();if(mention)dismissed.current={start:mention.start};close();return;}
        onKeyDown?.(event);
      }}
      onKeyUp={event=>{if(event.key!=='ArrowDown'&&event.key!=='ArrowUp')detect(event.currentTarget);}}
      onClick={event=>{detect(event.currentTarget);onClick?.(event);}}
      onPaste={event=>{onPaste?.(event);requestAnimationFrame(()=>detect(event.currentTarget));}}
      onFocus={event=>{onFocus?.(event);}}
      onBlur={event=>{close();onBlur?.(event);}}/>
    {open&&createPortal(<div ref={popup} id={id} className="mention-picker" role="dialog" aria-label="Project files and folders" style={position} onMouseDown={event=>event.preventDefault()}>
      <div className="mention-picker-heading"><Icon name="search" size={14}/><span>Files and folders</span><small>Type to filter</small></div>
      <MentionList id={`${id}-results`} label="Project files and folders" choices={choices} activeIndex={activeIndex} optionId={index=>`${id}-option-${index}`}
        className="mention-picker-results" optionClass="mention-option" activeClass="highlighted" optionAs="button"
        selected={selected} onHover={hover} onChoose={chooseChoice}
        empty={<p className="mention-picker-empty" role="status">{items.length?'No matching files or folders':'No project files yet'}</p>}/>
      <p className="mention-picker-hint">Enter or Tab adds the path. Esc closes.</p>
    </div>,document.body)}
  </>;
}
