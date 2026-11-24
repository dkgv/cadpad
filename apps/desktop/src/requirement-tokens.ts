/**
 * Requirement prose is a plain string in which a model mention is a token:
 * `@[Label](entry#builder)` or, for a part inside a model, `@[Label](entry#builder/part)`.
 * The label is display only, so renaming it never breaks a stored requirement.
 * Pure and dependency free so tests can load it directly.
 */
export type ModelTarget={entry:string;builder:string};
export type TokenTarget=ModelTarget&{part?:string};
export type TextSegment={kind:'text';text:string};
export type TokenSegment={kind:'token';label:string;entry:string;builder:string;part?:string;raw:string};
export type Segment=TextSegment|TokenSegment;

const TOKEN=/@\[([^\]\n]*)\]\(([^()\n]*)\)/g;
// Characters that would end a target early are escaped, so any entry path round-trips.
const escapeTarget=(value:string)=>value.replace(/[%()]/g,char=>'%'+char.charCodeAt(0).toString(16).toUpperCase().padStart(2,'0'));
const unescapeTarget=(value:string)=>value.replace(/%(25|28|29)/gi,(_,hex:string)=>String.fromCharCode(parseInt(hex,16)));
const cleanLabel=(label:string)=>label.replace(/[\[\]\s]+/g,' ').trim();

/** The token text for one mention. */
export function tokenFor(target:TokenTarget&{label:string}):string{
  const path=target.part?`${target.builder}/${target.part}`:target.builder;
  return `@[${cleanLabel(target.label)}](${escapeTarget(target.entry)}#${escapeTarget(path)})`;
}

function targetOf(body:string):TokenTarget|null{
  const hash=body.indexOf('#');
  if(hash<=0)return null;
  const entry=unescapeTarget(body.slice(0,hash)),path=unescapeTarget(body.slice(hash+1));
  const slash=path.indexOf('/');
  const builder=slash<0?path:path.slice(0,slash),part=slash<0?undefined:path.slice(slash+1);
  if(!entry||!builder||part==='')return null;
  return part?{entry,builder,part}:{entry,builder};
}

/** Split text into plain text and token segments. Malformed `@[..](..)` stays plain text. */
export function parseTokens(text:string):Segment[]{
  const segments:Segment[]=[];let last=0;
  for(const match of text.matchAll(TOKEN)){
    const target=targetOf(match[2]);
    if(!target)continue;
    const index=match.index??0;
    if(index>last)segments.push({kind:'text',text:text.slice(last,index)});
    segments.push({kind:'token',label:match[1],...target,raw:match[0]});
    last=index+match[0].length;
  }
  if(last<text.length)segments.push({kind:'text',text:text.slice(last)});
  return segments;
}

/** Inverse of `parseTokens`: tokens are rebuilt from their parts, so label edits are kept. */
export function serializeTokens(segments:Segment[]):string{
  return segments.map(segment=>segment.kind==='text'?segment.text:tokenFor(segment)).join('');
}

/** Distinct models mentioned in `text`, in order of first mention (parts count as their model). */
export function mentionedModels(text:string):ModelTarget[]{
  const seen=new Set<string>(),found:ModelTarget[]=[];
  for(const segment of parseTokens(text)){
    if(segment.kind!=='token')continue;
    const key=JSON.stringify([segment.entry,segment.builder]);
    if(seen.has(key))continue;
    seen.add(key);found.push({entry:segment.entry,builder:segment.builder});
  }
  return found;
}
