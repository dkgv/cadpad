/**
 * Pure helpers for the Requirements review UI (spec §2.2, §5, §6): wire types, span highlighting,
 * questions, review hashes, result and finding summaries, needs-review diffs and assistant prompts.
 * No React and no Tauri imports so tests can load it directly.
 */
import {mentionedModels,parseTokens,type ModelTarget} from './requirement-tokens';

export type Quantity={value:number;unit:string};
export type Span=[number,number];
export type PrintLine={text:string;claim:string;span?:Span|null;inferred:boolean;because?:string|null};
export type Print={title:string;sets:PrintLine[];lines:PrintLine[];numbers_chosen?:string|null;details:string[]};
export type Fix={kind:'ask_user';options?:string[];question?:string|null}|{kind:'edit_source';description:string;path?:string|null;proposed?:string|null}|{kind:'edit_ir';description:string;edits:{at:string;value?:unknown}[]};
export type Diagnostic={code:string;severity:'error'|'warning';at:string;span?:Span|null;message:string;fix?:Fix|null};
export type Hashes={requirement:string;manifest:string;plan:string};
export type RequirementStatus='draft'|'needs_review'|'error'|'pending'|'inconclusive'|'not_met'|'met_partial'|'met';
export type Approval='draft'|'approved'|'edited_since_approval';
export type RequirementInfo={
  id:string;name:string;status?:RequirementStatus|null;approval:Approval;text:string;draft_hash:string;
  /** The hashes of the compiled draft used for approval. */
  draft_hashes?:Hashes|null;
  print?:Print|null;rendered?:string|null;diagnostics:Diagnostic[];
  approved?:{hashes:Hashes;rendered:string;print:Print}|null;
  last_result?:unknown;rerun_required?:boolean;run?:RequirementRun|null;
};
export type ProsePrintAdvisory={available:boolean;reason?:string;draft_hash:string;draft_hashes:Hashes;warnings:string[]};
export type RequirementRun={id:string;state:string;requirement_id?:string;error?:{code?:string;message?:string}|null};
export type RequirementsReply={catalog:string;project_workspace?:string;requirements:RequirementInfo[];run?:RequirementRun|null};

// ------------------------------------------------------------------ spans

/** UTF-16 index of each Unicode scalar boundary: `offsets[i]` is where scalar `i` starts, `offsets[n]` is the text length. */
export function scalarOffsets(text:string):number[]{
  const offsets:number[]=[];
  for(let index=0;index<text.length;){offsets.push(index);index+=text.codePointAt(index)!>0xffff?2:1;}
  offsets.push(text.length);
  return offsets;
}
/** Slice by Unicode-scalar offsets (what core's spans use), not UTF-16 indices. */
export function sliceScalars(text:string,start:number,end:number,offsets=scalarOffsets(text)):string{
  const n=offsets.length-1,a=Math.max(0,Math.min(n,start)),b=Math.max(a,Math.min(n,end));
  return text.slice(offsets[a],offsets[b]);
}
export type Highlight={kind:'covered'|'inferred'|'uncovered'|'plain';raw:string;claim?:string};
const WORDY=/[\p{L}\p{N}]/u;

/**
 * Split the requirement text into highlighted runs. Covered: inside a printed rule's span.
 * Inferred: inside an inferred rule's span. Uncovered: words outside every span (never checked).
 * Chip tokens are never split: a token takes the highlight of its first character.
 */
export function highlightSegments(text:string,print?:Print|null,diagnostics:Diagnostic[]=[]):Highlight[]{
  const offsets=scalarOffsets(text),n=offsets.length-1;
  if(!n)return [];
  const kind=new Uint8Array(n),claims:string[]=new Array(n).fill('');
  const paint=(span:Span|null|undefined,value:number,claim:string)=>{
    if(!span||!Number.isInteger(span[0])||!Number.isInteger(span[1]))return;
    for(let i=Math.max(0,span[0]);i<Math.min(n,span[1]);i++){kind[i]=value;claims[i]=claim;}
  };
  const lines=[...(print?.sets??[]),...(print?.lines??[])];
  for(const line of lines)if(!line.inferred)paint(line.span,1,line.claim);
  for(const line of lines)if(line.inferred)paint(line.span,2,line.claim);
  // A token is a unit: copy the first scalar's label over its whole range.
  let utf16=0;const scalarAt=new Map<number,number>();offsets.forEach((offset,index)=>scalarAt.set(offset,index));
  for(const segment of parseTokens(text)){
    const length=segment.kind==='text'?segment.text.length:segment.raw.length;
    if(segment.kind==='token'){
      const first=scalarAt.get(utf16),last=scalarAt.get(utf16+length);
      if(first!==undefined&&last!==undefined)for(let i=first+1;i<last;i++){kind[i]=kind[first];claims[i]=claims[first];}
    }
    utf16+=length;
  }
  const out:Highlight[]=[];
  let start=0;
  const flush=(end:number)=>{
    if(end<=start)return;
    const raw=text.slice(offsets[start],offsets[end]);
    let label:Highlight['kind']=kind[start]===1?'covered':kind[start]===2?'inferred':'plain';
    if(label==='plain'&&WORDY.test(raw))label='uncovered';
    out.push(label==='covered'||label==='inferred'?{kind:label,raw,claim:claims[start]}:{kind:label,raw});
  };
  for(let i=1;i<=n;i++){
    if(i===n||kind[i]!==kind[start]||claims[i]!==claims[start]){flush(i);start=i;}
  }
  // Merge neighbouring runs of the same look.
  const merged:Highlight[]=[];
  for(const run of out){
    const last=merged[merged.length-1];
    if(last&&last.kind===run.kind&&last.claim===run.claim)last.raw+=run.raw;else merged.push({...run});
  }
  return merged;
}

// -------------------------------------------------------------- questions

export type Question={key:string;code:string;at:string;message:string;question:string;options:string[];span:Span|null;quote:string};
/** Diagnostics the assistant cannot settle alone: `ask_user` fixes and `E_VAGUE`. */
export function questionsOf(info:Pick<RequirementInfo,'text'|'diagnostics'>):Question[]{
  const offsets=scalarOffsets(info.text),out:Question[]=[];
  for(const diagnostic of info.diagnostics??[]){
    const ask=diagnostic.fix?.kind==='ask_user'?diagnostic.fix:null;
    if(!ask&&diagnostic.code!=='E_VAGUE')continue;
    const span=diagnostic.span?.length===2?diagnostic.span:null;
    out.push({
      key:`${diagnostic.code}|${diagnostic.at}|${span?.join('-')??''}`,code:diagnostic.code,at:diagnostic.at,message:diagnostic.message,
      question:ask?.question||diagnostic.message,options:[...(ask?.options??[])],span,
      quote:span?sliceScalars(info.text,span[0],span[1],offsets):'',
    });
  }
  return out;
}
export const errorsOf=(diagnostics:Diagnostic[]=[])=>diagnostics.filter(item=>item.severity==='error');

/** The prompt that returns the user's answers to the assistant, which records them in the IR. */
export function answersPrompt(info:Pick<RequirementInfo,'id'|'name'|'text'>,answers:{question:Question;answer:string;suggested:boolean}[]):string{
  const lines=answers.map(({question,answer,suggested},index)=>`${index+1}. ${question.quote?`About "${question.quote}" (${question.at}): `:`(${question.at}): `}${question.question}\n   My answer${suggested?' (your suggested answer)':''}: ${answer}`);
  return `Here are my answers to your questions about the requirement "${info.name}" (ID ${info.id}).\n${lines.join('\n')}\nUse control_requirements: list first, then record each answer in the IR \`answers\` and compile. Mark numbers provided only in these answers as inferred and cite the answer in the note. If more questions remain, save the draft with its diagnostics so I can answer in Requirements. Save the compiled draft when all questions are resolved. Do not run it and do not approve it: I review the print and approve it myself.`;
}

// ----------------------------------------------------------------- review

/** Reviewed hashes for `approve_requirement`, or null when core has not reported them for the draft. */
export function reviewHashes(info:RequirementInfo):Hashes|null{
  const hashes=info.draft_hashes;
  return hashes&&hashes.requirement&&hashes.manifest&&hashes.plan?hashes:null;
}
/** Why Approve is disabled, or null when it is allowed. */
export function approveBlock(info:RequirementInfo):string|null{
  const errors=errorsOf(info.diagnostics);
  if(errors.length)return `Fix ${errors.length} ${errors.length===1?'problem':'problems'} first: ${errors[0].message}`;
  if(!info.print||!info.rendered&&!info.print.lines.length)return 'There is nothing to review yet.';
  if(!reviewHashes(info))return 'The reviewed version is not available yet. Ask the assistant to save the requirement again.';
  return null;
}
/** The requirement is waiting on the user: never approved, or edited since. */
export const needsApproval=(info:RequirementInfo)=>info.approval!=='approved'&&!info.rerun_required;

export type DiffRow={sign:'+'|'-'|'~';text:string};
const lineText=(line:PrintLine)=>(line.inferred?'Inferred: ':'')+line.text;
/** Plain-language difference between the approved print and the current draft print (§6.3). */
export function printDiff(approved?:Print|null,draft?:Print|null):DiffRow[]{
  if(!approved||!draft)return [];
  const rows:DiffRow[]=[];
  const before=[...approved.sets,...approved.lines],after=[...draft.sets,...draft.lines];
  const used=new Set<number>();
  for(const line of after){
    const same=before.findIndex((old,index)=>!used.has(index)&&lineText(old)===lineText(line));
    if(same>=0){used.add(same);continue;}
    const twin=line.claim?before.findIndex((old,index)=>!used.has(index)&&old.claim===line.claim):-1;
    if(twin>=0){used.add(twin);rows.push({sign:'~',text:`Changed from "${lineText(before[twin])}" to "${lineText(line)}"`});}
    else rows.push({sign:'+',text:`Added: ${lineText(line)}`});
  }
  before.forEach((old,index)=>{if(!used.has(index))rows.push({sign:'-',text:`Removed: ${lineText(old)}`});});
  if((approved.numbers_chosen??'')!==(draft.numbers_chosen??''))rows.push({sign:'~',text:`Numbers I chose: ${draft.numbers_chosen||'none'} (was ${approved.numbers_chosen||'none'})`});
  return rows;
}

// ---------------------------------------------------------------- results

export type Finding={
  id:string;claim:string;origin_claim?:string;primitive?:string;a:string[];b:string[];first_state:number;last_state:number;
  states:{index:number;label:string;state?:unknown}[];state_count:number;worst_value?:Quantity|null;worst_margin?:Quantity|null;
  label?:'new'|'still_failing'|'resolved'|null;
};
export type Worst={state:number;state_label:string;pair:{a:string[];b:string[]};witness?:string[]|null;value?:Quantity|null;margin?:Quantity|null};
export type SlowPair={state:number;state_label:string;pair:{a:string[];b:string[]};reason?:string|null};
export type PrimitiveResult={
  id:string;claim:string;primitive:string;quantifier?:string;status:string;met?:boolean|null;margin?:Quantity|null;worst?:Worst|null;witness?:Worst|null;
  states:number;jobs?:number;failing_states:number[];failing_state_count?:number;inconclusive:SlowPair[];inconclusive_count?:number;errors:string[];error_count?:number;
};
export type Change={kind?:string;effect?:string;at?:string;message:string};
export type ResultView={
  status:string;unsupported:number;claims:{claim:string;rule:string;status:string}[];primitives:PrimitiveResult[];findings:Finding[];
  timings:{total_s:number;claims:Record<string,number>}|null;changes:Change[];reasons:string[];finishedAt:string|null;
};
const array=<T,>(value:unknown):T[]=>Array.isArray(value)?value as T[]:[];
const record=(value:unknown):Record<string,unknown>|null=>value&&typeof value==='object'&&!Array.isArray(value)?value as Record<string,unknown>:null;

/**
 * Read `last_result` whatever its wrapper: a bare `ResultBody`, `{body,timings,...}` (EvalResult) or either with
 * labelled `findings`, `relink` / `changes` / `reasons`. Missing or malformed data gives null, never a throw.
 */
export function readResult(raw:unknown):ResultView|null{
  const top=record(raw);if(!top)return null;
  const body=record(top.body)??top;
  if(typeof body.status!=='string')return null;
  const timings=record(top.timings);
  const relink=record(top.relink);
  const labelled=array<Finding>(top.findings_labelled).length?array<Finding>(top.findings_labelled):array<Finding>(top.findings).length?array<Finding>(top.findings):array<Finding>(body.findings);
  return {
    status:body.status,unsupported:typeof body.unsupported==='number'?body.unsupported:0,
    claims:array(body.claims),primitives:array<PrimitiveResult>(body.primitives).map(item=>({...item,failing_states:array<number>(item.failing_states),inconclusive:array<SlowPair>(item.inconclusive),errors:array<string>(item.errors)})),
    findings:labelled.map(item=>({...item,a:array<string>(item.a),b:array<string>(item.b),states:array(item.states)})),
    timings:timings&&typeof timings.total_s==='number'?{total_s:timings.total_s,claims:(record(timings.claims) as Record<string,number>)??{}}:null,
    changes:array<Change>(top.changes).length?array<Change>(top.changes):array<Change>(relink?.changes),
    reasons:array<string>(top.reasons),
    finishedAt:typeof top.finished_at==='string'?top.finished_at:null,
  };
}

const UNITS:Record<string,string>={mm:' mm',cm:' cm',m:' m',um:' µm',in:' in',deg:'°',rad:' rad',mm2:' mm²',cm2:' cm²',mm3:' mm³',cm3:' cm³',g:' g',kg:' kg',s:' s',N:' N',N_s:' N·s',one:''};
export function formatQuantity(quantity?:Quantity|null,signed=false):string{
  if(!quantity||!Number.isFinite(quantity.value))return '';
  const rounded=Number(quantity.value.toPrecision(3));
  const text=Math.abs(rounded)>=1000?String(Math.round(rounded)):String(rounded);
  return `${signed&&rounded>0?'+':''}${text}${UNITS[quantity.unit]??` ${quantity.unit}`}`;
}
export const pairText=(pair?:{a:string[];b:string[]}|null)=>pair?[partNames(pair.a),partNames(pair.b)].filter(Boolean).join(' and '):'';
const partName=(key:string)=>{const slash=key.indexOf('/',key.indexOf('#')+1);return slash<0?key:key.slice(slash+1);};
export const partNames=(keys:string[])=>keys.length>3?`${keys.slice(0,2).map(partName).join(', ')} and ${keys.length-2} more`:keys.map(partName).join(', ');
/** The model (`entry#builder`) a part key belongs to. */
export function modelOfPart(key:string):ModelTarget|null{
  const hash=key.indexOf('#');if(hash<=0)return null;
  const slash=key.indexOf('/',hash),head=slash<0?key:key.slice(0,slash);
  return {entry:head.slice(0,hash),builder:head.slice(hash+1)};
}

/** The primitive that explains the verdict: the worst failing one, else the tightest margin, else the first. */
export function headlinePrimitive(result:ResultView):PrimitiveResult|null{
  const margin=(item:PrimitiveResult)=>item.worst?.margin?.value??item.margin?.value??Infinity;
  const pick=(items:PrimitiveResult[])=>items.reduce<PrimitiveResult|null>((best,item)=>!best||margin(item)<margin(best)?item:best,null);
  const failing=result.primitives.filter(item=>item.status==='not_met'||item.met===false);
  return pick(failing)??pick(result.primitives);
}
export type StripCell={failing:boolean;label:string};
/** One cell per checked state (bucketed past `limit`), failing ones flagged, for the position strip. */
export function stateStrip(primitive:PrimitiveResult,limit=120):StripCell[]{
  const total=Math.max(0,primitive.states||0);if(!total)return [];
  const failing=new Set(primitive.failing_states),cells=Math.min(total,limit),out:StripCell[]=[];
  for(let cell=0;cell<cells;cell++){
    const from=Math.floor(cell*total/cells),to=Math.max(from+1,Math.floor((cell+1)*total/cells));
    let bad=false;for(let index=from;index<to&&!bad;index++)bad=failing.has(index);
    out.push({failing:bad,label:to-from>1?`states ${from+1}–${to}`:`state ${from+1}`});
  }
  return out;
}

export const STATUS_TEXT:Record<RequirementStatus,string>={draft:'Needs your OK',needs_review:'Needs review',error:"Couldn't check",pending:'Needs recheck',inconclusive:'Inconclusive',not_met:'Not met',met_partial:'Met, not fully',met:'Met'};
/** Sidebar sort: things that need action first, then waiting, then fine (results mock). */
const RANK:Record<string,number>={not_met:0,inconclusive:1,error:2,needs_review:3,draft:4,pending:5,met_partial:6,met:7};
export const statusRank=(status?:string|null)=>RANK[status??'']??5;
export const statusOf=(info:RequirementInfo):RequirementStatus=>(info.status??(info.approval==='draft'?'draft':'pending')) as RequirementStatus;
export const statusTone=(status:RequirementStatus)=>status==='met'||status==='met_partial'?'ok':status==='not_met'||status==='error'?'bad':status==='draft'||status==='needs_review'||status==='inconclusive'?'warn':'blue';
/** The shown result is a previous one, not about the current state. */
export const resultIsPrevious=(status:RequirementStatus)=>status==='draft'||status==='needs_review'||status==='error'||status==='pending';

/** Approved, current, and never run: there is no status to show yet. */
export const notRun=(info:RequirementInfo)=>info.approval==='approved'&&!info.status&&!info.rerun_required;
export const summaryOf=(infos:RequirementInfo[])=>{
  const counts=new Map<string,{label:string;count:number;rank:number}>();
  for(const info of infos){
    const status=statusOf(info),key=notRun(info)?'not_run':status;
    const entry=counts.get(key)??{label:key==='not_run'?'not checked':STATUS_TEXT[status].toLowerCase(),count:0,rank:key==='not_run'?5.5:statusRank(status)};
    entry.count++;counts.set(key,entry);
  }
  return [...counts.entries()].sort((a,b)=>a[1].rank-b[1].rank).map(([key,{label,count}])=>({key,label,count}));
};

// --------------------------------------------------------------- grouping

const modelId=(model:ModelTarget)=>JSON.stringify([model.entry,model.builder]);
/** Requirements keyed by every model their text mentions; those with no chip go under the empty key. */
export function groupByModel<T extends {text:string}>(items:T[]):Map<string,T[]>{
  const groups=new Map<string,T[]>();
  for(const item of items){
    const keys=mentionedModels(item.text).map(modelId);
    for(const key of keys.length?keys:['']){const list=groups.get(key)??[];list.push(item);groups.set(key,list);}
  }
  return groups;
}
export const groupKey=modelId;

// --------------------------------------------------------------- findings

export const FINDING_TEXT={new:'new',still_failing:'still failing',resolved:'resolved'} as const;
export const findingStates=(finding:Finding)=>finding.states.length?finding.first_state===finding.last_state||finding.states.length===1?finding.states[0].label:`${finding.states[0].label} to ${finding.states[finding.states.length-1].label}`:'';
export const canAccept=(finding:Finding)=>finding.label!=='resolved';
/** Toggle one id in a selection; resolved findings and unknown ids cannot be selected. */
export function toggleSelection(selected:string[],id:string):string[]{return selected.includes(id)?selected.filter(item=>item!==id):[...selected,id];}
export function pruneSelection(selected:string[],findings:Finding[]):string[]{
  const live=new Set(findings.filter(canAccept).map(item=>item.id));
  const next=selected.filter(id=>live.has(id));
  return next.length===selected.length?selected:next;
}
/** Batch request to the assistant: draft allowances for the selected clashes; the user then reviews one draft. */
export function allowancePrompt(info:Pick<RequirementInfo,'id'|'name'>,findings:Finding[]):string{
  const lines=findings.map((finding,index)=>`${index+1}. finding ${finding.id}: ${pairText(finding)} (claim ${finding.claim}), failing ${findingStates(finding)||'at some states'}${finding.worst_value?`, worst ${formatQuantity(finding.worst_value)}`:''}`);
  return `I accept ${findings.length===1?'this clash':'these clashes'} as intended in the requirement "${info.name}" (ID ${info.id}):\n${lines.join('\n')}\nUse control_requirements: list first, then draft ONE allowance per clash (all in a single edit of the IR \`allow\`, as narrow as the failing states and a limit just above the worst value; mark the numbers inferred and note why), compile, and save the result as a draft. Do not approve it and do not run it: I review the print with the new allowances once and approve it myself.`;
}
export function removeRulePrompt(info:Pick<RequirementInfo,'id'|'name'>,line:PrintLine):string{
  return `In the requirement "${info.name}" (ID ${info.id}), remove the inferred rule "${line.text}" (claim ${line.claim}). Use control_requirements: list first, edit the IR, compile, and save it as a draft. Do not approve it: I review the print myself.`;
}
