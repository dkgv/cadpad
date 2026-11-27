import React,{useRef,useState} from 'react';

/**
 * One row in an `@` mention picker. Both composers (the chat prompt and the
 * requirement definition) present their suggestions through this shape so the
 * filtering, keyboard order and row markup stay identical.
 */
export type MentionChoice={
  /** Stable identity for React keys and for mapping a choice back to its source. */
  key:string;
  label:string;
  detail?:string;
  /** Optional leading icon, e.g. folder/file in the Assistant composer. */
  icon?:React.ReactNode;
  /** Tooltip text; defaults to the label. */
  title?:string;
  /** Text used for filtering; defaults to label + detail. */
  search?:string;
  /** Sort priority when no filter is typed; lower comes first. */
  rank?:number;
};

const MAX_SCORE=Number.MAX_SAFE_INTEGER;

/**
 * Lowercase and treat path/word separators as word breaks, so `v1_frame_name`,
 * `v1-frame-name` and `v1 frame name` are all searched the same way.
 */
export function normalizeMention(value:string){
  return value.toLocaleLowerCase().replace(/[\\/._-]+/g,' ').replace(/\s+/g,' ').trim();
}

function isSubsequence(needle:string,haystack:string){
  let at=0;
  for(const char of needle){
    at=haystack.indexOf(char,at);
    if(at<0)return false;
    at++;
  }
  return true;
}

type WordHit={type:number;at:number};

/** How one query token matches one word; lower `type` is stronger. */
function wordHit(word:string,token:string):WordHit|null{
  if(word===token)return{type:0,at:0};
  const at=word.indexOf(token);
  if(at===0)return{type:1,at};
  if(at>0)return{type:2,at};
  // Only allow typos (subsequence) for longer tokens; short ones would match too much.
  if(token.length>=3&&isSubsequence(token,word))return{type:3,at:word.indexOf(token[0])};
  return null;
}

/**
 * Every query token must match some word of `text`, in any order. Lower score
 * is better. Returns null when at least one token has no match.
 */
function matchTokens(text:string,tokens:string[]){
  const words=text.split(' ').filter(Boolean);
  let score=0;
  for(const token of tokens){
    let best:WordHit|null=null;
    for(const word of words){
      const hit=wordHit(word,token);
      if(hit&&(!best||hit.type<best.type||(hit.type===best.type&&hit.at<best.at)))best=hit;
    }
    if(!best)return null;
    score+=best.type*1_000+best.at;
  }
  return score;
}

function matchesText(text:string,needle:string,tokens:string[]){
  return text.includes(needle)||matchTokens(text,tokens)!==null;
}

/** True when `choice` satisfies `query` (any word order, fuzzy longer tokens). */
export function matchesMention(choice:MentionChoice,query:string){
  const needle=normalizeMention(query);
  if(!needle)return true;
  return matchesText(normalizeMention(choice.search??`${choice.label} ${choice.detail??''}`),needle,needle.split(' '));
}

/**
 * Case-insensitive filter over each choice's searchable text. Words may appear
 * in any order (`frame v1` matches `v1_frame_name`), path segments count, and
 * longer tokens tolerate typos via subsequence matching.
 */
export function filterMentions(choices:MentionChoice[],query:string):MentionChoice[]{
  const needle=normalizeMention(query);
  if(!needle)return choices;
  return choices.filter(choice=>matchesMention(choice,query));
}

/**
 * Lower is better. Whole-label matches (exact, then prefix) win, then unordered
 * word matches in the label, then detail matches, then any search text. Within a
 * band, stronger word hits (exact > prefix > substring > fuzzy) and earlier
 * occurrences win.
 */
export function scoreMention(choice:MentionChoice,query:string):number{
  if(!query)return 0;
  const needle=normalizeMention(query);
  if(!needle)return 0;
  const tokens=needle.split(' ');
  const label=normalizeMention(choice.label);
  const detail=normalizeMention(choice.detail??'');
  const haystack=normalizeMention(choice.search??`${choice.label} ${choice.detail??''}`);
  if(!matchesText(haystack,needle,tokens))return MAX_SCORE;
  let band:number,fuzzy=0;
  if(label===needle)band=0;
  else if(label.startsWith(needle))band=1;
  else{
    const labelMatch=matchTokens(label,tokens);
    if(labelMatch!==null){band=2;fuzzy=labelMatch;}
    else if(detail===needle)band=3;
    else if(detail.startsWith(needle))band=4;
    else{
      const detailMatch=matchTokens(detail,tokens);
      if(detailMatch!==null){band=5;fuzzy=detailMatch;}
      else{
        const haystackMatch=matchTokens(haystack,tokens);
        if(haystackMatch===null)return MAX_SCORE;
        band=6;fuzzy=haystackMatch;
      }
    }
  }
  const at=haystack.indexOf(needle);
  // Bands must dominate; clamp the finer signals into their own slots.
  return band*1_000_000_000+Math.min(fuzzy,999_999)*1_000+Math.min(Math.max(at,0),999);
}

/**
 * Without a query, open tabs come first. With one, best matches come first
 * (open tabs break ties), so typing "rail" puts `parts/rail.py` above
 * `parts/trailing.py`.
 */
export function orderMentions(choices:MentionChoice[],query=''):MentionChoice[]{
  const key=(choice:MentionChoice)=>choice.rank??Number.MAX_SAFE_INTEGER;
  if(!query)return [...choices].sort((a,b)=>key(a)-key(b));
  return choices
    .map((choice,index)=>({choice,index,score:scoreMention(choice,query)}))
    .sort((a,b)=>a.score-b.score||key(a.choice)-key(b.choice)||a.index-b.index)
    .map(item=>item.choice);
}

/** `-1` means nothing is selected. Clamp a real index into range. */
export function clampIndex(index:number,length:number){
  if(index<0)return -1;
  return Math.min(index,Math.max(length-1,0));
}

/**
 * Wrap `delta` steps around the list; used by ArrowUp/ArrowDown. From "nothing
 * selected" (-1), Down picks the first row and Up picks the last.
 */
export function stepIndex(index:number,length:number,delta:number){
  if(!length)return -1;
  if(index<0)return delta>0?0:length-1;
  return ((index+delta)%length+length)%length;
}

/**
 * Cursor + multi-selection for a mention list. `active` is the row the cursor
 * sits on (`initialActive` when reset, `-1` when nothing is selected); `selected`
 * holds every row picked by Shift+Arrow so Enter can insert them all.
 */
export function useMentionSelection(length:number,initialActive=-1){
  const initialSelected=()=>initialActive>=0?new Set([initialActive]):new Set<number>();
  const [active,setActive]=useState(initialActive);
  const [selected,setSelected]=useState<Set<number>>(initialSelected);
  const anchor=useRef(initialActive);
  const reset=()=>{anchor.current=initialActive;setActive(initialActive);setSelected(initialSelected());};
  const move=(delta:number,extend=false)=>{
    if(!length)return;
    const next=extend
      ?active<0?(delta>0?0:length-1):Math.min(Math.max(active+delta,0),length-1)
      :stepIndex(active,length,delta);
    if(extend){
      const from=anchor.current>=0?anchor.current:active>=0?active:next;
      anchor.current=from;
      const low=Math.min(from,next),high=Math.max(from,next);
      setSelected(new Set(Array.from({length:high-low+1},(_,index)=>low+index)));
    }else{
      anchor.current=next;
      setSelected(new Set([next]));
    }
    setActive(next);
  };
  const hover=(index:number)=>{
    anchor.current=index;
    // Pointer moves fire repeatedly within one row; avoid a state change per pixel.
    if(index===active&&selected.size===1&&selected.has(index))return;
    setActive(index);
    setSelected(new Set([index]));
  };
  return {active,selected,move,hover,reset};
}

type Placement={minWidth?:number;maxWidth?:number;maxHeight?:number;gap?:number;margin?:number};

/**
 * Fixed-position popup under (or above) an anchor. Both mention pickers go wider
 * than their input when needed so model names and file paths are not cut off.
 */
export function mentionPopupPosition(rect:DOMRect,{minWidth=320,maxWidth=680,maxHeight=320,gap=6,margin=12}:Placement={}):React.CSSProperties{
  const width=Math.min(maxWidth,Math.max(minWidth,rect.width),window.innerWidth-margin*2);
  const below=window.innerHeight-rect.bottom-margin;
  const above=rect.top-margin;
  const down=below>=Math.min(280,above);
  return {
    position:'fixed',
    right:'auto',
    left:Math.max(margin,Math.min(rect.left,window.innerWidth-width-margin)),
    top:down?rect.bottom+gap:undefined,
    bottom:down?undefined:window.innerHeight-rect.top+gap,
    width,
    maxHeight:Math.max(120,Math.min(maxHeight,down?below:above)),
  };
}

type Props={
  /** id and role=listbox target, referenced by the editor's `aria-controls`. */
  id:string;
  label:string;
  choices:MentionChoice[];
  activeIndex:number;
  optionId:(index:number)=>string;
  /** Class for the listbox element. */
  className:string;
  /** Class for each row; `activeClass` is appended to the highlighted row. */
  optionClass:string;
  activeClass:string;
  empty:React.ReactNode;
  onHover:(index:number)=>void;
  onChoose:(choice:MentionChoice)=>void;
  onMouseDown?:(event:React.MouseEvent)=>void;
  /** Rows picked with Shift+Arrow; when omitted the active row is selected alone. */
  selected?:ReadonlySet<number>;
  style?:React.CSSProperties;
  /** Buttons in the Assistant popup, divs inline in the requirement composer. */
  optionAs?:'button'|'div';
};

/** Shared listbox used by every `@` mention picker. */
export function MentionList({id,label,choices,activeIndex,optionId,className,optionClass,activeClass,empty,onHover,onChoose,onMouseDown,selected,style,optionAs='div'}:Props){
  const render=(choice:MentionChoice,index:number)=>{
    const active=index===activeIndex;
    const picked=active||(selected?.has(index)??false);
    const common={
      id:optionId(index),
      role:'option' as const,
      'aria-selected':picked,
      'data-mention-index':index,
      className:`${optionClass}${picked?` ${activeClass}`:''}`,
      title:choice.title??choice.label,
      // Pointer movement selects; an appearing popup under a stationary cursor does not.
      onPointerMove:()=>onHover(index),
      onClick:()=>onChoose(choice),
    };
    const body=<>{choice.icon}<span>{choice.label}</span>{choice.detail&&<small>{choice.detail}</small>}</>;
    return optionAs==='button'
      ?<button key={choice.key} type="button" {...common}>{body}</button>
      :<div key={choice.key} {...common}>{body}</div>;
  };
  return <div className={className} id={id} role="listbox" aria-label={label} aria-multiselectable={selected?'true':undefined} style={style} onMouseDown={onMouseDown}>
    {choices.length?choices.map(render):empty}
  </div>;
}
