import React,{useState} from 'react';

type Vector=[number,number,number];
type Motor={joint_id:string;target:number;unit:string;stiffness:number;damping:number;max_force:number};
type Load={force_n:Vector;torque_nm:Vector};
type Body={type:'fixed'|'dynamic';friction:number;restitution:number;mass?:{type:'kilograms';mass_kg:number}|{type:'density';density_kg_m3:number}};
type Check={type:'displacement';body:string;direction:Vector;minimum_mm:number}
  |{type:'coordinate';joint_id:string;target:number;unit:string;tolerance:number}
  |{type:'push';pusher:string;receiver:string;driver_joint:string;direction:Vector;minimum_mm:number;minimum_additional_mm:number;minimum_sampled_impulse_ns:number};
export type MotionTestDefinition={name:string;intent:string;initial_coordinates?:Record<string,number>;simulation?:{gravity_m_s2:Vector;bodies:Record<string,Body>};actions:{ticks:number;motors:Motor[];loads:Record<string,Load>}[];checks:Check[]};
export type MotionTestMeasurement={label:string;actual:number;expected:number;unit:string;passed:boolean};
export type MotionTestOutcome={passed:boolean;measurements:MotionTestMeasurement[]};
export type MotionTestReview={requirement:string;checks:{statement:string;met:boolean|null;measurements:MotionTestMeasurement[]}[];summary:string};
const number=(value:number)=>Number(value.toPrecision(8)).toString();
const name=(value:string)=>value.replaceAll('_',' ');
const vector=(value:Vector)=>`(${value.map(number).join(', ')})`;
function coordinate(value:number,unit:string){
  return unit==='m'?`${number(value*1000)} mm`:unit==='rad'?`${number(value*180/Math.PI)}°`:`${number(value)} ${unit}`;
}
function direction(value:Vector){
  const axes=['X','Y','Z'];
  const axis=value.findIndex(component=>Math.abs(component)===1);
  return axis>=0&&value.filter((_,index)=>index!==axis).every(component=>component===0)
    ?`along world ${value[axis]<0?'−':'+'}${axes[axis]}`:`along world direction ${vector(value)}`;
}
function checkCopy(check:Check){
  return check.type==='displacement'
    ?<>{name(check.body)} finishes at least {number(check.minimum_mm)} mm from its starting position, {direction(check.direction)}.</>
    :check.type==='coordinate'
    ?<>{name(check.joint_id)} finishes at {coordinate(check.target,check.unit)}, within ±{coordinate(check.tolerance,check.unit)}.</>
    :<><strong>{name(check.pusher)} pushes {name(check.receiver)}.</strong> The receiver finishes at least {number(check.minimum_mm)} mm from its starting position, {direction(check.direction)}, and moves at least {number(check.minimum_additional_mm)} mm farther than in the same simulation with the {name(check.driver_joint)} motor disabled. Direct pushing contact must reach a sampled normal impulse of at least {number(check.minimum_sampled_impulse_ns)} N s.</>;
}
function measurementCount(check:Check){return check.type==='push'?3:1;}
function partitionMeasurements(checks:Check[],measurements:MotionTestMeasurement[]|undefined){
  if(!measurements)return{slices:checks.map(()=>null as MotionTestMeasurement[]|null),remainder:[] as MotionTestMeasurement[]};
  let offset=0;
  const slices=checks.map(check=>{
    const count=measurementCount(check);
    const slice=measurements.slice(offset,offset+count);
    offset+=count;
    return slice.length?slice:null;
  });
  return{slices,remainder:measurements.slice(offset)};
}
function Measurements({items,check}:{items:MotionTestMeasurement[];check?:Check}){
  return <ul className="test-measurements">{items.map((measurement,index)=>{
    const isCoordinate=check?.type==='coordinate';
    // Coordinate results store remaining tolerance, rather than the joint value.
    const actual=isCoordinate?check.tolerance-measurement.actual:measurement.actual;
    const expected=isCoordinate?check.tolerance:measurement.expected;
    const value=(amount:number)=>coordinate(amount,measurement.unit);
    return <li key={index} className={measurement.passed?'passed':'failed'}><span>{isCoordinate?'Position error':name(measurement.label)}</span><small>Measured {value(actual)} · {isCoordinate?'maximum':'minimum'} {value(expected)}</small></li>;
  })}</ul>;
}
export function MotionTestDefinitionView({definition,review=null,outcome=null,previous=false}:{definition:MotionTestDefinition;review?:MotionTestReview|null;outcome?:MotionTestOutcome|null;previous?:boolean}){
  const [technicalOpen,setTechnicalOpen]=useState(false);
  let elapsed=0;
  const{slices,remainder}=partitionMeasurements(definition.checks,outcome?.measurements);
  const measured=!!outcome||!!review?.checks.some(check=>check.met!==null);
  const scored=review?review.checks.filter(check=>check.met!==null):slices.filter(Boolean) as MotionTestMeasurement[][];
  const passedChecks=review?review.checks.filter(check=>check.met===true).length:(scored as MotionTestMeasurement[][]).filter(slice=>slice.every(item=>item.passed)).length;
  const summary=review?`${review.summary}${previous?' · previous run':''}`:measured?`${scored.length?`${passedChecks} of ${scored.length} requirements met`:`${outcome!.measurements.filter(item=>item.passed).length} of ${outcome!.measurements.length} checks passed`}${previous?' · previous run':''}`:'';
  return <div className="test-definition">
    <div className="test-checks-heading">
      <h4>{measured?'Results against requirements':'Review checks'}</h4>
      {measured&&<span className="muted">{summary}</span>}
    </div>
    <ol className="test-criteria">{(review?review.checks.map((check,index)=>{
      const state=check.met===null?'pending':check.met?'passed':'failed';
      return <li key={index} className={`test-criterion ${state}`}>
        <div className="test-criterion-copy">{check.statement}</div>
        {check.measurements.length>0&&<div className="test-criterion-result" aria-label={state==='passed'?'Requirement met':'Requirement not met'}>
          <span className={`test-criterion-status ${state}`}>{state==='passed'?'Met':'Not met'}</span>
          <Measurements items={check.measurements} check={definition.checks[index]}/>
        </div>}
      </li>;
    }):definition.checks.map((check,index)=>{
      const items=slices[index];
      const state=!items?'pending':items.every(item=>item.passed)?'passed':'failed';
      return <li key={index} className={`test-criterion ${state}`}>
        <div className="test-criterion-copy">{checkCopy(check)}</div>
        {items&&<div className="test-criterion-result" aria-label={state==='passed'?'Requirement met':'Requirement not met'}>
          <span className={`test-criterion-status ${state}`}>{state==='passed'?'Met':'Not met'}</span>
          <Measurements items={items} check={check}/>
        </div>}
      </li>;
    }))}</ol>
    {!review&&!!remainder.length&&<Measurements items={remainder}/>}
    <details className="test-setup"><summary>Setup and steps</summary>{definition.checks.some(check=>check.type==='push')&&<p className="muted">Contact impulse is sampled at the last solver substep. It is not a force measurement.</p>}
      <ol className="test-sequence">{definition.actions.map((action,index)=>{
        const start=elapsed;elapsed+=action.ticks/120;
        return <li key={index}><strong>{number(start)}–{number(elapsed)} s</strong>
          {action.motors.map(motor=><p key={motor.joint_id}>Drive {name(motor.joint_id)} toward {coordinate(motor.target,motor.unit)}, limited to {number(motor.max_force)} {motor.unit==='rad'?'N m':'N'}. Spring stiffness {number(motor.stiffness)} {motor.unit==='rad'?'N m/rad':'N/m'}, damping {number(motor.damping)} {motor.unit==='rad'?'N m s/rad':'N s/m'}.</p>)}
          {!action.motors.length&&<p>{index?'Keep previous motor targets.':'No motor targets.'}</p>}
          {Object.entries(action.loads).map(([body,load])=><p key={body}>{name(body)}: world force {vector(load.force_n)} N at its centre of mass; torque {vector(load.torque_nm)} N m.</p>)}
          {!Object.keys(action.loads).length&&<p>No external loads.</p>}
        </li>;
      })}</ol>
      <p className="muted">Motor targets persist until replaced. Each step replaces the external loads. Simulation advances at 120 steps per second.</p>
      {!!Object.keys(definition.initial_coordinates??{}).length&&<><h4>Starting pose</h4><ul>{Object.entries(definition.initial_coordinates!).map(([joint,value])=>{
        const check=definition.checks.find(check=>check.type==='coordinate'&&check.joint_id===joint);
        const unit=check?.type==='coordinate'?check.unit:definition.actions.flatMap(action=>action.motors).find(motor=>motor.joint_id===joint)?.unit;
        return <li key={joint}>{name(joint)}: {unit?coordinate(value,unit):`${number(value)} (SI coordinate)`}</li>;
      })}</ul><p className="muted">Coordinates without an explicit saved unit use SI: metres for sliders and radians for hinges.</p></>}
      {definition.simulation&&<><h4>Simulation setup</h4><p>World gravity {vector(definition.simulation.gravity_m_s2)} m/s².</p><ul>{Object.entries(definition.simulation.bodies).map(([id,body])=><li key={id}>{name(id)}: {body.type==='fixed'?'fixed':body.mass?.type==='kilograms'?`${number(body.mass.mass_kg)} kg`:body.mass?.type==='density'?`density ${number(body.mass.density_kg_m3)} kg/m³`:'dynamic'}; friction {number(body.friction)}, restitution {number(body.restitution)}.</li>)}</ul></>}
    <details onToggle={event=>setTechnicalOpen(event.currentTarget.open)}><summary>Technical definition</summary>{technicalOpen&&<pre>{JSON.stringify(definition,null,2)}</pre>}</details>
    </details>
  </div>;
}
