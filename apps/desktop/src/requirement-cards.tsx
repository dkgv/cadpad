import React,{useEffect,useMemo,useState} from 'react';
import {parseTokens} from './requirement-tokens';
import {
  STATUS_TEXT,FINDING_TEXT,allowancePrompt,answersPrompt,approveBlock,canAccept,errorsOf,findingStates,formatQuantity,headlinePrimitive,highlightSegments,needsApproval,notRun,
  pairText,partNames,printDiff,pruneSelection,questionsOf,readResult,removeRulePrompt,resultIsPrevious,sliceScalars,stateStrip,statusOf,statusTone,toggleSelection,
  type Finding,type PrimitiveResult,type Question,type RequirementInfo,type RequirementRun,type ResultView,type Worst,
} from './requirement-review';

export type CardActions={
  busy:string|null;agentBusy:boolean;run:RequirementRun|null;
  approve:(info:RequirementInfo)=>void;remove:(info:RequirementInfo)=>void;start:(info:RequirementInfo)=>void;cancel:(runId:string)=>void;
  /** Send a prompt to the assistant (the only way an edit to a requirement happens). */
  ask:(prompt:string,info:RequirementInfo)=>Promise<void>|void;
  editWording:(info:RequirementInfo)=>void;
  replay:(info:RequirementInfo,primitiveId:string,sample:Worst)=>Promise<void>;
};

/** Requirement text with `@[Label](entry#builder)` tokens drawn as chips. */
export function ChipText({raw}:{raw:string}){
  return <>{parseTokens(raw).map((segment,index)=>segment.kind==='text'?<React.Fragment key={index}>{segment.text}</React.Fragment>:<span key={index} className="req-chip" title={segment.part?`${segment.entry} / ${segment.part}`:segment.entry}>{segment.label}</span>)}</>;
}
/** Capitalise a plain word; names like `clamp_2` stay as written (mirrors core's print). */
const sentence=(text:string)=>/^\p{L}+(\s|$)/u.test(text)?text.charAt(0).toUpperCase()+text.slice(1):text;
const Badge=({status,children}:{status:ReturnType<typeof statusOf>;children?:React.ReactNode})=><span className={`rq-badge ${statusTone(status)}`}>{children??STATUS_TEXT[status]}</span>;

// ---------------------------------------------------------------- questions

/**
 * One question at a time (P1.75). Answers go to the assistant as a single message; the user never edits the IR.
 * `E_VAGUE` / `ask_user` diagnostics supply the question and options; the first option is the suggestion.
 */
export function QuestionFlow({info,questions,agentBusy,onSend}:{info:RequirementInfo;questions:Question[];agentBusy:boolean;onSend:(prompt:string)=>Promise<void>|void}){
  const signature=questions.map(item=>item.key).join('\n');
  const [answers,setAnswers]=useState<Record<string,{answer:string;suggested:boolean}>>({});
  const [reopened,setReopened]=useState<string|null>(null),[choice,setChoice]=useState(''),[sent,setSent]=useState(''),[error,setError]=useState('');
  useEffect(()=>{setAnswers({});setReopened(null);setError('');},[info.id,signature]);
  const open=reopened?questions.find(item=>item.key===reopened):questions.find(item=>!answers[item.key]);
  useEffect(()=>{if(open)setChoice(answers[open.key]?.answer??open.options[0]??'');},[open?.key]);
  const position=open?questions.indexOf(open)+1:questions.length;
  async function finish(next:Record<string,{answer:string;suggested:boolean}>){
    setError('');
    try{
      await onSend(answersPrompt(info,questions.map(question=>({question,...next[question.key]}))));
      setSent(signature);
    }catch(value){setError(value instanceof Error?value.message:String(value));}
  }
  function answer(question:Question,value:string,suggested:boolean){
    const next={...answers,[question.key]:{answer:value,suggested}};
    setAnswers(next);setReopened(null);
    if(questions.every(item=>next[item.key]))void finish(next);
  }
  function useSuggested(){
    const next={...answers};
    for(const question of questions)if(!next[question.key]&&question.options.length)next[question.key]={answer:question.options[0],suggested:true};
    setAnswers(next);setReopened(null);
    if(questions.every(item=>next[item.key]))void finish(next);
  }
  const answered=questions.filter(item=>answers[item.key]);
  const remaining=questions.filter(item=>!answers[item.key]);
  if(sent===signature)return <section className="rq-card rq-question" aria-label="Question sent"><p role="status" className="muted">Your answers went to the assistant. It will update the requirement for you to review.</p></section>;
  return <section className="rq-card rq-question" aria-label={`Question about ${info.name}`}>
    <div className="rq-head"><strong>Quick question</strong><span className="muted" aria-live="polite">{open?`${position} of ${questions.length}`:'All answered'}</span></div>
    {answered.length>0&&<ul className="rq-answered" aria-label="Your answers so far">{answered.map(item=><li key={item.key}><span>{item.quote||item.at}: <b>{answers[item.key].answer}</b></span><button type="button" className="rq-link" onClick={()=>setReopened(item.key)}>Change</button></li>)}</ul>}
    {open&&<form onSubmit={event=>{event.preventDefault();if(choice.trim())answer(open,choice.trim(),!!open.options.length&&choice===open.options[0]);}}>
      {open.quote&&<p className="rq-quote" aria-label="Your sentence"><ChipText raw={sliceScalars(info.text,0,open.span![0])}/><mark><ChipText raw={open.quote}/></mark><ChipText raw={sliceScalars(info.text,open.span![1],Infinity)}/></p>}
      <fieldset className="rq-options"><legend className="rq-question-text">{open.question}</legend>
        {open.options.map((option,index)=><label key={option} className={choice===option?'on':''}><input type="radio" name={`q-${open.key}`} value={option} checked={choice===option} onChange={()=>setChoice(option)} autoFocus={index===0}/><span>{option}{index===0&&<small className="rq-suggested">Suggested</small>}</span></label>)}
        {!open.options.length&&<input type="text" className="rq-answer-text" aria-label="Your answer" value={choice} onChange={event=>setChoice(event.target.value)} placeholder="Your answer" autoFocus/>}
      </fieldset>
      <div className="rq-actions"><button type="submit" className="primary-button" disabled={!choice.trim()||agentBusy}>{remaining.length>1||reopened?'Next':'Send answers'}</button>
        {remaining.length>1&&remaining.every(item=>item.options.length)&&<button type="button" onClick={useSuggested}>Use suggested for the rest</button>}</div>
    </form>}
    {error&&<p role="alert" className="error">{error}</p>}
  </section>;
}

// ------------------------------------------------------------------ review

function ReviewBody({info,actions,previous}:{info:RequirementInfo;actions:CardActions;previous?:ResultView|null}){
  const [linked,setLinked]=useState<string|null>(null);
  const print=info.print,lines=[...(print?.sets??[]),...(print?.lines??[])];
  const segments=useMemo(()=>highlightSegments(info.text,print,info.diagnostics),[info.text,print,info.diagnostics]);
  const uncovered=segments.filter(item=>item.kind==='uncovered');
  const warnings=(info.diagnostics??[]).filter(item=>item.severity==='warning'&&item.code!=='W_COVER_TEXT');
  const errors=errorsOf(info.diagnostics);
  const edited=info.approval==='edited_since_approval';
  const diff=edited?printDiff(info.approved?.print,print):[];
  const numbered=(print?.lines??[]);
  return <>
    <span className="rq-label">You wrote</span>
    <p className="rq-prose" aria-label="Your requirement with what was understood">{segments.map((item,index)=>{
      const on=!!item.claim&&linked===item.claim;
      if(item.kind==='covered'||item.kind==='inferred')return <span key={index} className={`cov${item.kind==='inferred'?' inf':''}${on?' on':''}`} tabIndex={0} data-claim={item.claim} aria-label={item.kind==='inferred'?'Read between the lines':'Understood'} onMouseEnter={()=>setLinked(item.claim??null)} onMouseLeave={()=>setLinked(null)} onFocus={()=>setLinked(item.claim??null)} onBlur={()=>setLinked(null)}><ChipText raw={item.raw}/></span>;
      if(item.kind==='uncovered')return <span key={index} className="unc" title="Not checked: Cadpad can't measure this"><ChipText raw={item.raw}/></span>;
      return <span key={index}><ChipText raw={item.raw}/></span>;
    })}</p>
    {print&&<div className="rq-legend" aria-hidden="true"><span><i className="l-cov"/>understood</span><span><i className="l-inf"/>read between the lines</span><span><i className="l-unc"/>not checked</span></div>}
    {uncovered.length>0&&<div className="rq-callout warn" role="note"><div><b>{uncovered.map(item=>`“${item.raw.replace(/^[\s,.;:]+|[\s,.;:]+$/g,'')}”`).join(' and ')} won’t be checked.</b> Cadpad can’t measure {uncovered.length===1?'it':'them'}, so the result will say so.</div></div>}
    {errors.length>0&&<div className="rq-callout bad" role="alert"><div><b>{errors.length===1?'One problem':`${errors.length} problems`} to fix before approval.</b><ul>{errors.map((item,index)=><li key={index}>{item.message}</li>)}</ul></div></div>}
    {warnings.map((item,index)=><div key={index} className="rq-callout warn" role="note"><div>{item.message}</div></div>)}
    {edited&&diff.length>0&&<><span className="rq-label">What changed since you approved</span><DiffTable rows={diff}/></>}
    {print&&<><span className="rq-label">Cadpad will check</span>
      {print.sets.length>0&&<ul className="rq-sets">{print.sets.map((line,index)=><li key={index}>{line.text}</li>)}</ul>}
      <ol className="rq-print">{numbered.map((line,index)=><li key={`${line.claim}-${index}`} className={linked&&linked===line.claim?'on':''} data-claim={line.claim} onMouseEnter={()=>setLinked(line.claim)} onMouseLeave={()=>setLinked(null)}>
        <details><summary><span>{line.inferred?line.text:sentence(line.text)}{line.inferred&&<> <span className="rq-src inf">inferred</span>{line.because&&<span className="rq-why">Not in your words. Reason: {line.because}.</span>}</>}</span></summary>
          {line.inferred&&<div className="rq-detail"><button type="button" className="rq-link" disabled={actions.agentBusy} onClick={()=>void actions.ask(removeRulePrompt(info,line),info)}>Ask the assistant to remove this rule</button></div>}
        </details></li>)}</ol>
      {print.numbers_chosen&&<p className="rq-numbers"><b>Numbers I chose:</b> {print.numbers_chosen}</p>}
      {print.details.length>0&&<details className="rq-fine"><summary>Details <span className="muted">(automatic, nothing to do)</span></summary><ul className="rq-detail">{print.details.map((item,index)=><li key={index}>{item}</li>)}</ul></details>}
    </>}
    {previous&&edited&&<PreviousLine result={previous} info={info}/>}
  </>;
}
const DiffTable=({rows}:{rows:{sign:string;text:string}[]})=><table className="rq-diff" aria-label="What changed since you approved"><tbody>{rows.map((row,index)=><tr key={index}><td className={row.sign==='+'?'add':row.sign==='-'?'rem':''} aria-label={row.sign==='+'?'added':row.sign==='-'?'removed':'changed'}>{row.sign}</td><td>{row.text}</td></tr>)}</tbody></table>;

function PreviousLine({result,info}:{result:ResultView;info:RequirementInfo}){
  const primitive=headlinePrimitive(result),margin=primitive?.worst?.margin??primitive?.margin;
  return <p className="rq-prev">Last result (approved version): <b>{STATUS_TEXT[result.status as keyof typeof STATUS_TEXT]??result.status}</b>{margin?`, margin ${formatQuantity(margin)}`:''}{info.rerun_required?' · from an imported file':''}</p>;
}

/** Review of a draft (never approved, or edited since) and the single Approve (P1.76, P1.79). */
function ReviewCard({info,actions,previous}:{info:RequirementInfo;actions:CardActions;previous:ResultView|null}){
  const block=approveBlock(info),edited=info.approval==='edited_since_approval';
  return <>
    <ReviewBody info={info} actions={actions} previous={previous}/>
    <div className="rq-actions">
      <button type="button" className="primary-button" disabled={!!block||actions.busy===info.id} aria-describedby={block?`block-${info.id}`:undefined} onClick={()=>actions.approve(info)}>{actions.busy===info.id?'Approving…':edited?'Re-approve':'Approve'}</button>
      <button type="button" disabled={actions.agentBusy} onClick={()=>actions.editWording(info)}>Edit wording</button>
    </div>
    {block&&<p className="rq-hint" id={`block-${info.id}`}>{block}</p>}
    {!block&&(info.print?.lines??[]).some(line=>line.inferred)&&<p className="rq-hint">Approving also confirms the inferred {(info.print?.lines??[]).filter(line=>line.inferred).length===1?'rule':'rules'} above.</p>}
  </>;
}

/** Approved, but the model or the library changed what it means (P1.79). Nothing re-ran. */
function NeedsReviewCard({info,result,actions}:{info:RequirementInfo;result:ResultView|null;actions:CardActions}){
  const block=approveBlock(info);
  const rows=[...printDiff(info.approved?.print,info.print),...(result?.changes??[]).map(change=>({sign:'~',text:change.message})),...(result?.reasons??[]).map(text=>({sign:'~',text}))];
  return <>
    <p className="rq-lede">The model changed what this requirement covers. The last approved result is shown as it was.</p>
    {rows.length>0?<DiffTable rows={rows}/>:<p className="muted">The requirement changed since you approved it.</p>}
    <div className="rq-callout warn" role="note"><div>Cadpad did not re-check. Approve the new meaning first, so a result is never about something you haven’t read.</div></div>
    {result&&<PreviousLine result={result} info={info}/>}
    <div className="rq-actions">
      <button type="button" className="primary-button" disabled={!!block||actions.busy===info.id} aria-describedby={block?`block-${info.id}`:undefined} onClick={()=>actions.approve(info)}>{actions.busy===info.id?'Approving…':'Re-approve'}</button>
      <button type="button" disabled={actions.agentBusy} onClick={()=>actions.editWording(info)}>Edit wording</button>
    </div>
    {block&&<p className="rq-hint" id={`block-${info.id}`}>{block}</p>}
    {(info.rendered||info.approved?.rendered)&&<details className="rq-fine"><summary>See full print</summary><pre className="rq-pre">{info.rendered??info.approved?.rendered}</pre></details>}
  </>;
}

// ----------------------------------------------------------------- results

function ShowInViewer({worst,actions,info,primitiveId}:{worst:Worst|null;actions:CardActions;info:RequirementInfo;primitiveId:string}){
  const [opening,setOpening]=useState(false),[failed,setFailed]=useState('');
  if(!worst)return null;
  const sample=worst;
  async function open(){
    setOpening(true);
    setFailed('');
    try{await actions.replay(info,primitiveId,sample);}
    catch(value){setFailed(value&&typeof value==='object'&&'message' in value?String(value.message):String(value));}
    finally{setOpening(false);}
  }
  return <><button type="button" className="rq-mini" disabled={opening} onClick={()=>void open()}>{opening?'Preparing replay…':'Show in viewer'}</button>{failed&&<p role="alert" className="error">{failed}</p>}</>;
}

function Strip({primitive}:{primitive:PrimitiveResult}){
  const cells=stateStrip(primitive);
  if(!cells.length)return null;
  return <div className="rq-strip" role="img" aria-label={`${primitive.failing_state_count??primitive.failing_states.length} of ${primitive.states} checked states fail`}>{cells.map((cell,index)=><i key={index} className={cell.failing?'f':''} title={cell.label}/>)}</div>;
}

function FindingsList({info,findings,actions}:{info:RequirementInfo;findings:Finding[];actions:CardActions}){
  const [selected,setSelected]=useState<string[]>([]),[requested,setRequested]=useState<string[]|null>(null),[error,setError]=useState('');
  const ids=findings.map(item=>item.id).join('|');
  useEffect(()=>{setSelected(current=>pruneSelection(current,findings));},[ids]);
  const chosen=findings.filter(item=>selected.includes(item.id));
  const open=findings.filter(item=>item.label!=='resolved').length;
  async function accept(){
    setError('');
    try{await actions.ask(allowancePrompt(info,chosen),info);setRequested(chosen.map(item=>item.id));setSelected([]);}
    catch(value){setError(value instanceof Error?value.message:String(value));}
  }
  return <section className="rq-findings" aria-label="Clashes">
    <span className="rq-label">{open===1?'1 clash':`${open} clashes`}</span>
    <ul>{findings.map(finding=><li key={finding.id} className={`rq-finding${finding.label==='resolved'?' resolved':''}`}>
      {canAccept(finding)?<input type="checkbox" aria-label={`Select ${pairText(finding)} to accept as allowance`} checked={selected.includes(finding.id)} onChange={()=>setSelected(current=>toggleSelection(current,finding.id))}/>:<span className="rq-check-space"/>}
      <div><span className={`rq-tag ${finding.label??'new'}`}>{FINDING_TEXT[finding.label??'new']}</span> <span className="rq-finding-title">{pairText(finding)}</span>
        <div className="rq-kv">{finding.label==='resolved'?'was failing, fine now':`${finding.worst_value?`overlap ${formatQuantity(finding.worst_value)} `:''}at ${findingStates(finding)||'some states'}`}</div>
        {finding.label!=='resolved'&&<ShowInViewer worst={finding.states[0]?{state:finding.states[0].index,state_label:finding.states[0].label,pair:finding}:null} actions={actions} info={info} primitiveId={finding.claim}/>}
      </div></li>)}</ul>
    {chosen.length>0&&<div className="rq-actions"><button type="button" className="primary-button rq-mini" disabled={actions.agentBusy} onClick={()=>void accept()}>Accept {chosen.length} as allowance{chosen.length===1?'':'s'}</button><button type="button" className="rq-mini" onClick={()=>setSelected([])}>Clear</button></div>}
    {requested&&<p className="rq-callout blue" role="status"><span>Asked the assistant to draft {requested.length===1?'an allowance':`${requested.length} allowances`}. {requested.length===1?'It':'They'} come back as one draft for you to review and approve. Nothing is active until then.</span></p>}
    {error&&<p role="alert" className="error">{error}</p>}
  </section>;
}

function ResultBody({info,result,previous,actions}:{info:RequirementInfo;result:ResultView|null;previous:boolean;actions:CardActions}){
  const status=statusOf(info);
  if(!result)return <p className="rq-lede muted">{status==='pending'?'Not checked yet.':'No result yet.'}</p>;
  const primitive=headlinePrimitive(result),worst=primitive?.worst??primitive?.witness??null;
  const margin=worst?.margin??primitive?.margin;
  const failing=primitive?(primitive.failing_state_count??primitive.failing_states.length):0;
  const slow=result.primitives.flatMap(item=>item.inconclusive);
  const issues=result.primitives.flatMap(item=>item.errors);
  const changes=result.changes.filter(change=>change.effect==='run_changed');
  const shown=result.status;
  return <div className={previous?'rq-previous':''}>
    {previous&&<p className="rq-prev-label">Previous result</p>}
    {!previous&&changes.length>0&&<details className="rq-fine"><summary>Changed since approval</summary><ul>{changes.map((change,index)=><li key={index}>{change.message}</li>)}</ul></details>}
    {(shown==='met'||shown==='met_partial')&&<><div className="rq-meter"><span className="big">{margin?formatQuantity(margin):'Met'}</span><span className="muted">{margin?'spare at the tightest point':'every check passed'}</span></div>
      {worst&&<p className="rq-kv">Tightest at <b>{worst.state_label}</b>, {pairText(worst.pair)}{primitive?` · ${primitive.states} ${primitive.states===1?'state':'states'} checked`:''}</p>}
      {result.unsupported>0&&<div className="rq-callout warn" role="note"><div>{result.unsupported} {result.unsupported===1?'phrase':'phrases'} not checked. See “not checked” in the review.</div></div>}</>}
    {shown==='not_met'&&primitive&&<><p className="rq-headline">{failing===1?'Fails at 1 position':`Fails at ${failing} of ${primitive.states} positions`}{margin&&margin.value<0?`, missing by ${formatQuantity({...margin,value:-margin.value})}`:''}.</p>
      {worst&&<p className="rq-kv">Worst: <b>{worst.state_label}</b>, {pairText(worst.pair)}{worst.value?` (${formatQuantity(worst.value)})`:''}</p>}
      <Strip primitive={primitive}/>
      {!previous&&worst&&<div className="rq-actions"><ShowInViewer worst={worst} actions={actions} info={info} primitiveId={primitive.id}/></div>}</>}
    {shown==='inconclusive'&&<><p className="rq-headline">Cadpad could not decide.</p>
      <p className="rq-kv">{slow.length===1?'One check ran out of time':`${slow.length||'Some'} checks ran out of time`}, so nothing is claimed for {slow.length===1?'it':'them'}. The other checks are reported as usual.</p>
      {slow.length>0&&<ul className="rq-slow">{slow.slice(0,5).map((item,index)=><li key={index}>{pairText(item.pair)} at {item.state_label}{item.reason?`: ${item.reason}`:''}</li>)}{slow.length>5&&<li>and {slow.length-5} more</li>}</ul>}</>}
    {shown==='error'&&<><p className="rq-headline">The check hit an error.</p>{issues.slice(0,3).map((item,index)=><p key={index} className="rq-kv">{item}</p>)}</>}
    {!['met','met_partial','not_met','inconclusive','error'].includes(shown)&&<p className="rq-kv">Last result: {shown}.</p>}
    {!previous&&result.findings.length>0&&<FindingsList info={info} findings={result.findings} actions={actions}/>}
    <details className="rq-fine"><summary>Details</summary><div className="rq-detail">
      {result.timings&&<p>Checked in {result.timings.total_s.toFixed(1)} s{Object.keys(result.timings.claims).length>0?` · ${Object.entries(result.timings.claims).map(([claim,seconds])=>`${claim} ${seconds.toFixed(1)} s`).join(', ')}`:''}</p>}
      {result.primitives.length>0&&<p>{result.primitives.reduce((sum,item)=>sum+(item.jobs??0),0)} checks over {result.primitives.length} {result.primitives.length===1?'rule':'rules'}</p>}
      {result.claims.length>0&&<ul>{result.claims.map(claim=><li key={claim.claim}>{claim.claim}: {claim.status.replace('_',' ')}</li>)}</ul>}
      {!result.timings&&!result.claims.length&&<p className="muted">No timings recorded.</p>}
    </div></details>
  </div>;
}

function ResultCard({info,result,actions}:{info:RequirementInfo;result:ResultView|null;actions:CardActions}){
  const status=statusOf(info),run=actions.run;
  const mine=run&&(!run.requirement_id||run.requirement_id===info.id)?run:info.run;
  const active=!!mine&&['queued','running','cancelling'].includes(mine.state);
  const previous=resultIsPrevious(status)||active;
  return <>
    {active&&mine&&<div className="rq-run" role="status"><span className="spin" aria-hidden="true"/><strong>{mine.state==='queued'?'Queued':mine.state==='cancelling'?'Stopping':'Checking…'}</strong><button type="button" className="rq-link" onClick={()=>actions.cancel(mine.id)}>Cancel</button></div>}
    {status==='pending'&&!notRun(info)&&!active&&result?.reasons[0]&&<p className="rq-hint">{result.reasons[0]}</p>}
    {status==='error'&&!result&&<p className="rq-lede">Cadpad couldn’t check this requirement. Fix the model, then try again.</p>}
    <ResultBody info={info} result={result} previous={previous} actions={actions}/>
    {mine&&!active&&mine.state!=='completed'&&<p className="rq-hint" role="status">{mine.state==='cancelled'?'Check stopped. The previous result is kept.':`Check could not finish: ${mine.error?.message??'it failed'}`}</p>}
    <div className="rq-actions">
      {!active&&<button type="button" className="rq-mini" disabled={actions.busy===info.id} onClick={()=>actions.start(info)}>{status==='error'?'Try again':result&&!previous?'Check again':'Check now'}</button>}
    </div>
  </>;
}

// -------------------------------------------------------------------- card

/** One requirement in the sidebar, whichever screen its status calls for. */
export function RequirementCard({info,actions}:{info:RequirementInfo;actions:CardActions}){
  const status=statusOf(info),result=useMemo(()=>readResult(info.last_result),[info.last_result]);
  // Draft or edited since approval: the user has something to read. Approved but changed by the model: re-approve.
  const unapproved=info.approval!=='approved';
  const questions=useMemo(()=>unapproved?questionsOf(info):[],[info.text,info.diagnostics,unapproved]);
  const review=unapproved&&!questions.length,relink=!unapproved&&status==='needs_review';
  return <article className="rq-card" aria-label={`Requirement ${info.name}`}>
    <div className="rq-head"><strong>{info.name}</strong><Badge status={status}>{notRun(info)?'Not checked':undefined}</Badge></div>
    {questions.length>0&&<p className="rq-lede">Waiting for your answer. Answer the question at the top of this panel.</p>}
    {review&&<ReviewCard info={info} actions={actions} previous={result}/>}
    {relink&&<NeedsReviewCard info={info} result={result} actions={actions}/>}
    {!unapproved&&!relink&&<ResultCard info={info} result={result} actions={actions}/>}
    <div className="rq-footer"><span className="rq-spacer"/><button type="button" className="icon-button" aria-label={`Delete requirement ${info.name}`} title="Delete requirement" disabled={actions.busy===info.id} onClick={()=>actions.remove(info)}>×</button></div>
  </article>;
}
