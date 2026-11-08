import test from 'node:test';
import assert from 'node:assert/strict';
import {selectedAnchor,selectedCircle,type Inspection} from '../src/references.ts';

const selection={revision:'r1',instance_id:'plate',entity_id:'edge:4'};
const reference={entity_kind:'edge' as const,format:'occt-brep-v3-no-display-v1' as const,sha256:'a'.repeat(64)};
const anchor={kind:'geometry_center' as const,instance_id:'plate',entity:reference};
const circle={kind:'geometry_circle' as const,instance_id:'plate',entity:reference,measure:'diameter' as const};
const inspection:Inspection={revision:'r1',pose_version:0,entities:[{reference:selection,anchor,circular_definition:circle}]};

test('unnamed selections use inspected fingerprints while semantic names remain preferred',()=>{
  const part={instance_id:'plate',features:{}};
  assert.deepEqual(selectedAnchor(selection,part,inspection),anchor);
  assert.deepEqual(selectedCircle(selection,part,inspection,'radius'),{...circle,measure:'radius'});
  const semantic={instance_id:'plate',features:{bore:['edge:4']}};
  assert.deepEqual(selectedAnchor(selection,semantic,inspection),{kind:'feature_center',instance_id:'plate',feature:'bore'});
  assert.deepEqual(selectedCircle(selection,semantic,inspection,'diameter'),{kind:'diameter',instance_id:'plate',feature:'bore'});
  assert.deepEqual(selectedAnchor({...selection,entity_id:'part:0'},part,null),{kind:'bounds',instance_id:'plate',fraction:[0.5,0.5,0.5]});
});
test('late, wrong-entity and wrong-instance inspection replies cannot create measurement anchors',()=>{
  const part={instance_id:'plate'};
  for(const reply of [null,{...inspection,revision:'old'},{...inspection,entities:[{...inspection.entities![0],reference:{...selection,entity_id:'edge:5'}}]},{...inspection,entities:[{...inspection.entities![0],anchor:{...anchor,instance_id:'other'},circular_definition:{...circle,instance_id:'other'}}]}]){
    assert.equal(selectedAnchor(selection,part,reply),null);
    assert.equal(selectedCircle(selection,part,reply,'diameter'),null);
  }
  assert.equal(selectedAnchor(selection,{instance_id:'other'},inspection),null);
  assert.equal(selectedCircle(selection,{instance_id:'other'},inspection,'radius'),null);
});
test('malformed geometry fingerprints and noncircular selections cannot be pinned as circles',()=>{
  const invalid={...inspection,entities:[{reference:selection,anchor:{...anchor,entity:{...reference,sha256:'invalid'}}}]};
  assert.equal(selectedAnchor(selection,{instance_id:'plate'},invalid),null);
  assert.equal(selectedCircle(selection,{instance_id:'plate'},invalid,'diameter'),null);
});
