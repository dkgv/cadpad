//! Revision-bound recordings. Playback selects accepted samples; it never solves
//! new geometry, resumes a recorded simulation, or edits design coordinates.
use crate::{
    Core, Error, Result,
    kinematics::{Evaluation, Mechanism, Pose},
    motion, portable,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc, time::Instant};

const MAX_FRAMES: usize = 3600;
const MAX_BODY_FRAMES: usize = 32768;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORDINGS: usize = 16;
const TOTAL_BYTES: usize = 64 * 1024 * 1024;
fn invalid(message: impl Into<String>) -> Error {
    Error::domain("INVALID_RECORDING", message)
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub time_s: f64,
    pub pose_version: u64,
    pub poses: BTreeMap<String, Pose>,
    pub coordinates: Vec<f64>,
    pub constraints: Option<crate::constraints::Report>,
}
impl Sample {
    pub(crate) fn capture(time_s: f64, pose_version: u64, evaluation: Evaluation) -> Self {
        Self {
            time_s,
            pose_version,
            poses: evaluation.poses,
            coordinates: evaluation.joints.iter().map(|j| j.coordinate).collect(),
            constraints: evaluation.constraints,
        }
    }
    fn evaluation(&self, mechanism: &Mechanism) -> Result<Evaluation> {
        // Samples follow published (topological) joint order. The internal
        // forward evaluator indexes coordinates by definition order instead.
        let published = mechanism.evaluate()?.joints;
        let coordinates = mechanism
            .definition()
            .joints
            .iter()
            .map(|joint| {
                let index = published
                    .iter()
                    .position(|state| state.id == joint.id)
                    .ok_or_else(|| invalid("Missing recorded joint"))?;
                self.coordinates
                    .get(index)
                    .copied()
                    .ok_or_else(|| invalid("Missing recorded coordinate"))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut result = mechanism.evaluate_at(&coordinates, true)?;
        result.poses = self.poses.clone();
        result.constraints = self.constraints.clone();
        // Joint input is disabled during playback. Published axes/origins still
        // follow the recorded parent placement, including dynamic constraint error.
        for state in &mut result.joints {
            let joint = mechanism
                .definition()
                .joints
                .iter()
                .find(|joint| joint.id == state.id)
                .ok_or_else(|| invalid("Missing recorded joint definition"))?;
            let parent = joint
                .parent
                .as_ref()
                .map_or(glam::DAffine3::IDENTITY, |id| {
                    let pose = &self.poses[id];
                    glam::DAffine3::from_rotation_translation(
                        glam::DQuat::from_array(pose.quaternion),
                        glam::DVec3::from_array(pose.position) * 0.001,
                    )
                });
            let attachment = parent * joint.parent_frame.transform()?;
            state.origin_mm = (attachment.translation * 1000.).to_array();
            let axis = match &joint.kind {
                crate::kinematics::JointKind::Prismatic { axis }
                | crate::kinematics::JointKind::Revolute { axis } => Some(*axis),
                _ => None,
            };
            if let Some(axis) = axis {
                state.axis_world = attachment
                    .transform_vector3(glam::DVec3::from_array(axis))
                    .normalize()
                    .to_array();
            }
            state.drivable = false;
        }
        Ok(result)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub revision: String,
    pub input_fingerprint: String,
    pub source_mode: String,
    pub digest: String,
    pub bytes: usize,
    pub frame_count: usize,
    pub duration_s: f64,
    pub capacity_reached: bool,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Recording {
    schema_version: u32,
    revision: String,
    input_fingerprint: String,
    source_mode: String,
    samples: Vec<Sample>,
}
#[derive(Clone)]
pub(crate) struct Recorder {
    name: String,
    mode: String,
    started: Instant,
    initial_sim_time: Option<f64>,
    capacity: usize,
    frames: Vec<Arc<Sample>>,
    full: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Status {
    pub name: String,
    pub frame_count: usize,
    pub duration_s: f64,
    pub capacity: usize,
    pub capacity_reached: bool,
}
#[derive(Clone)]
pub(crate) struct Playback {
    metadata: Metadata,
    recording: Arc<Recording>,
    index: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Selection {
    pub recording_id: String,
    pub name: String,
    pub frame_index: usize,
    pub frame_count: usize,
    pub time_s: f64,
    pub duration_s: f64,
    pub source_mode: String,
}
impl Recorder {
    fn new(name: String, snapshot: &motion::Snapshot) -> Result<Self> {
        if name.trim().is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
            return Err(invalid(
                "Recording name must be 1..120 printable characters",
            ));
        }
        if snapshot.evaluation.poses.is_empty() {
            return Err(invalid("Build geometry before recording"));
        }
        let capacity = MAX_FRAMES.min(MAX_BODY_FRAMES / snapshot.evaluation.poses.len());
        if capacity < 2 {
            return Err(invalid("Too many instances for a bounded recording"));
        }
        Ok(Self {
            name,
            mode: snapshot.mode.clone(),
            started: Instant::now(),
            initial_sim_time: snapshot.simulation.as_ref().map(|s| s.current.time_s),
            capacity,
            frames: vec![Arc::new(Sample::capture(
                0.,
                snapshot.pose_version,
                snapshot.evaluation.clone(),
            ))],
            full: false,
        })
    }
    pub(crate) fn capture(
        &mut self,
        evaluation: Evaluation,
        version: u64,
        simulation_time: Option<f64>,
    ) {
        if self.frames.len() >= self.capacity {
            self.full = true;
            return;
        }
        let time_s = match (simulation_time, self.initial_sim_time) {
            (Some(time), Some(initial)) => time - initial,
            _ => self.started.elapsed().as_secs_f64(),
        };
        let time_s = time_s.max(self.frames.last().unwrap().time_s);
        self.frames
            .push(Arc::new(Sample::capture(time_s, version, evaluation)));
    }
    pub(crate) fn status(&self) -> Status {
        Status {
            name: self.name.clone(),
            frame_count: self.frames.len(),
            duration_s: self.frames.last().unwrap().time_s,
            capacity: self.capacity,
            capacity_reached: self.full,
        }
    }
}
impl Playback {
    pub(crate) fn test_trace(
        test_id: &str,
        name: &str,
        revision: &str,
        samples: Vec<Sample>,
        index: usize,
        mechanism: &Mechanism,
    ) -> Result<Self> {
        let record = Recording {
            schema_version: 1,
            revision: revision.into(),
            input_fingerprint: "0".repeat(64),
            source_mode: "simulate".into(),
            samples,
        };
        let metadata = Metadata {
            id: format!("motion-test:{test_id}"),
            name: name.into(),
            revision: revision.into(),
            input_fingerprint: record.input_fingerprint.clone(),
            source_mode: "simulate".into(),
            digest: "0".repeat(64),
            bytes: 0,
            frame_count: record.samples.len(),
            duration_s: record
                .samples
                .last()
                .ok_or_else(|| invalid("Empty test trace"))?
                .time_s,
            capacity_reached: false,
        };
        if index >= record.samples.len() || record.samples.len() > MAX_FRAMES {
            return Err(invalid("Test frame out of range"));
        }
        validate_samples(&record, &metadata, Some(mechanism))?;
        record.samples[index].evaluation(mechanism)?;
        Ok(Self {
            metadata,
            recording: Arc::new(record),
            index,
        })
    }
    pub(crate) fn evaluation(&self, mechanism: &Mechanism) -> Result<Evaluation> {
        self.recording.samples[self.index].evaluation(mechanism)
    }
    pub(crate) fn selection(&self) -> Selection {
        Selection {
            recording_id: self.metadata.id.clone(),
            name: self.metadata.name.clone(),
            frame_index: self.index,
            frame_count: self.recording.samples.len(),
            time_s: self.recording.samples[self.index].time_s,
            duration_s: self.metadata.duration_s,
            source_mode: self.metadata.source_mode.clone(),
        }
    }
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    List,
    StartRecording {
        name: String,
    },
    FinishRecording,
    CancelRecording,
    Select {
        recording_id: String,
        frame_index: usize,
    },
    Leave,
    Delete {
        recording_id: String,
    },
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub workspace_id: String,
    pub revision: String,
    pub expected_pose_version: u64,
    pub command: Command,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Reply {
    pub snapshot: motion::Snapshot,
    pub recordings: Vec<Metadata>,
    pub frame_times_s: Option<Vec<f64>>,
}
fn revision(ws: &crate::Workspace, args: &Control) -> Result<()> {
    if ws.stored.current != args.revision {
        return Err(Error::domain(
            "STALE_ENTITY",
            "Recording revision is no longer current",
        ));
    }
    Ok(())
}
fn version(state: &motion::State, args: &Control) -> Result<u64> {
    if state.version != args.expected_pose_version {
        return Err(Error::domain(
            "POSE_CONFLICT",
            "Recording operation requires the current pose version",
        ));
    }
    state
        .version
        .checked_add(1)
        .filter(|v| *v <= 9_007_199_254_740_991)
        .ok_or_else(|| Error::domain("POSE_CONFLICT", "Pose version exhausted"))
}
fn validate_samples(
    record: &Recording,
    metadata: &Metadata,
    mechanism: Option<&Mechanism>,
) -> Result<()> {
    let instance_ids = if let Some(mechanism) = mechanism {
        mechanism.evaluate()?.poses.into_keys().collect::<Vec<_>>()
    } else {
        record.samples[0].poses.keys().cloned().collect()
    };
    let coordinate_count = mechanism.map_or(record.samples[0].coordinates.len(), |m| {
        m.definition().joints.len()
    });
    if instance_ids.is_empty() || coordinate_count > 128 {
        return Err(invalid("Invalid recording body/joint count"));
    }
    let mut previous = -1.;
    let mut body_frames = 0;
    for sample in &record.samples {
        body_frames += sample.poses.len();
        if !sample.time_s.is_finite()
            || sample.time_s < previous
            || sample.coordinates.len() != coordinate_count
            || sample.coordinates.iter().any(|q| !q.is_finite())
            || sample.poses.keys().ne(instance_ids.iter())
            || sample.poses.values().any(|p| {
                p.position.iter().any(|v| !v.is_finite() || v.abs() > 1e9)
                    || p.quaternion.iter().any(|v| !v.is_finite())
                    || (p.quaternion.iter().map(|v| v * v).sum::<f64>() - 1.).abs() > 1e-6
            })
        {
            return Err(invalid("Invalid recorded sample"));
        }
        previous = sample.time_s;
    }
    if body_frames > MAX_BODY_FRAMES || record.samples.last().unwrap().time_s != metadata.duration_s
    {
        return Err(invalid("Recording capacity/duration mismatch"));
    }
    if record.samples[0].time_s != 0.
        || !matches!(record.source_mode.as_str(), "manipulate" | "simulate")
    {
        return Err(invalid("Invalid recording time origin/mode"));
    }
    Ok(())
}
fn decode_recording(
    bytes: &[u8],
    metadata: &Metadata,
    mechanism: Option<&Mechanism>,
) -> Result<Recording> {
    if uuid::Uuid::parse_str(&metadata.id).is_err()
        || uuid::Uuid::parse_str(&metadata.revision).is_err()
        || !portable::valid_hash(&metadata.input_fingerprint)
        || !portable::valid_hash(&metadata.digest)
        || metadata.name.trim().is_empty()
        || metadata.name.len() > 120
        || metadata.name.chars().any(char::is_control)
        || metadata.bytes > MAX_BYTES
        || bytes.len() != metadata.bytes
        || portable::bytes_hash(bytes) != metadata.digest
    {
        return Err(invalid("Recording digest or manifest mismatch"));
    }
    let record: Recording = serde_json::from_slice(bytes)?;
    if record.schema_version != 1
        || record.revision != metadata.revision
        || record.input_fingerprint != metadata.input_fingerprint
        || record.source_mode != metadata.source_mode
        || record.samples.len() != metadata.frame_count
        || record.samples.is_empty()
        || record.samples.len() > MAX_FRAMES
    {
        return Err(invalid("Recording metadata mismatch"));
    }
    validate_samples(&record, metadata, mechanism)?;
    Ok(record)
}
pub(crate) fn validate_recording_bytes(bytes: &[u8], metadata: &Metadata) -> Result<()> {
    decode_recording(bytes, metadata, None).map(|_| ())
}
pub(crate) fn rebase_recording(
    bytes: &[u8],
    metadata: &Metadata,
    revision: &str,
    mechanism: &Mechanism,
) -> Result<(Metadata, Vec<u8>)> {
    let mut record = decode_recording(bytes, metadata, Some(mechanism))?;
    record.revision = revision.into();
    let bytes = serde_json::to_vec(&record)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Rebound recording exceeds 8 MiB"));
    }
    let mut metadata = metadata.clone();
    metadata.revision = revision.into();
    metadata.digest = portable::bytes_hash(&bytes);
    metadata.bytes = bytes.len();
    Ok((metadata, bytes))
}
pub(crate) fn metadata_fingerprint(
    recordings: &BTreeMap<String, Metadata>,
) -> Result<Option<String>> {
    if recordings.is_empty() {
        Ok(None)
    } else {
        Ok(Some(portable::bytes_hash(&serde_json::to_vec(recordings)?)))
    }
}
impl Core {
    pub async fn control_playback(&self, actor: &str, args: Control) -> Result<Reply> {
        let _publication = self.blob_publication.read().await;
        self.ensure_actor_active(actor)?;
        // Serialize disk work on bounded private copies outside the workspace lock.
        let (saved, loaded, cached, mechanism) = {
            let mut workspaces = self.workspaces.lock().await;
            let ws = workspaces
                .get_mut(&args.workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            revision(ws, &args)?;
            version(motion::state(ws)?, &args)?;
            let fingerprint = portable::fingerprint(&ws.stored.revisions[&args.revision].inputs)?;
            match &args.command {
                Command::FinishRecording => {
                    let recorder = motion::state(ws)?
                        .recorder
                        .clone()
                        .ok_or_else(|| invalid("No recording is active"))?;
                    if ws.stored.recordings.len() >= MAX_RECORDINGS {
                        return Err(invalid("Delete a recording before saving more than 16"));
                    }
                    (Some((recorder, fingerprint)), None, None, None)
                }
                Command::Select { recording_id, .. } => {
                    if ws.stored.revisions[&args.revision].model["rebuild_required"] == true {
                        return Err(Error::domain(
                            "UNSUPPORTED_OPERATION",
                            "Build the saved design before opening its recordings",
                        ));
                    }
                    if motion::state(ws)?.recorder.is_some() {
                        return Err(invalid("Finish or cancel recording before playback"));
                    }
                    let metadata = ws
                        .stored
                        .recordings
                        .get(recording_id)
                        .cloned()
                        .ok_or_else(|| invalid("Unknown recording in this workspace"))?;
                    if metadata.revision != args.revision
                        || metadata.input_fingerprint != fingerprint
                    {
                        return Err(Error::domain(
                            "STALE_ENTITY",
                            "Recording belongs to another design revision",
                        ));
                    }
                    let state = motion::state(ws)?;
                    let cached = state
                        .playback
                        .as_ref()
                        .filter(|p| p.metadata.id == *recording_id)
                        .map(|p| (p.metadata.clone(), p.recording.clone()));
                    if cached.is_some() {
                        (None, None, cached, None)
                    } else {
                        (None, Some(metadata), None, Some(state.mechanism.clone()))
                    }
                }
                _ => (None, None, None, None),
            }
        };
        let directory = self.root.join("blobs");
        let revision_id = args.revision.clone();
        let cancel = self.actor_signal(actor);
        let (saved, loaded) = tokio::task::spawn_blocking(move || -> Result<_> {
            if *cancel.borrow() {
                return Err(Error::domain("CANCELLED", "Recording operation cancelled"));
            }
            let saved = if let Some((recorder, fingerprint)) = saved {
                let record = Recording {
                    schema_version: 1,
                    revision: revision_id,
                    input_fingerprint: fingerprint.clone(),
                    source_mode: recorder.mode.clone(),
                    samples: recorder.frames.iter().map(|s| s.as_ref().clone()).collect(),
                };
                let bytes = serde_json::to_vec(&record)?;
                if bytes.len() > MAX_BYTES {
                    return Err(invalid("Recording exceeds the 8 MiB storage limit"));
                }
                let digest = portable::bytes_hash(&bytes);
                let metadata = Metadata {
                    id: crate::id(),
                    name: recorder.name,
                    revision: record.revision.clone(),
                    input_fingerprint: fingerprint,
                    source_mode: recorder.mode,
                    digest: digest.clone(),
                    bytes: bytes.len(),
                    frame_count: record.samples.len(),
                    duration_s: record.samples.last().unwrap().time_s,
                    capacity_reached: recorder.full,
                };
                portable::store_blobs(&directory, &BTreeMap::from([(digest, bytes)]))?;
                Some(metadata)
            } else {
                None
            };
            let loaded = if let Some(metadata) = loaded {
                if !portable::valid_hash(&metadata.digest) || metadata.bytes > MAX_BYTES {
                    return Err(invalid("Invalid recording manifest"));
                }
                let bytes = portable::read_bounded(&directory.join(&metadata.digest), MAX_BYTES)?;
                if bytes.len() != metadata.bytes || portable::bytes_hash(&bytes) != metadata.digest
                {
                    return Err(invalid("Recording digest mismatch"));
                }
                let record = decode_recording(&bytes, &metadata, mechanism.as_ref())?;
                Some((metadata, Arc::new(record)))
            } else {
                None
            };
            Ok((saved, loaded.or(cached)))
        })
        .await
        .map_err(|_| invalid("Recording storage task failed"))??;
        let mut workspaces = self.workspaces.lock().await;
        self.ensure_actor_active(actor)?;
        let ws = workspaces
            .get_mut(&args.workspace_id)
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
        revision(ws, &args)?;
        let next_version = version(motion::state(ws)?, &args)?;
        let mut frame_times_s = None;
        let mutating = !matches!(args.command, Command::List);
        let recordings_changed = matches!(
            args.command,
            Command::FinishRecording | Command::Delete { .. }
        );
        match args.command {
            Command::List => {}
            Command::StartRecording { name } => {
                let state = motion::state(ws)?;
                if state.recorder.is_some() || state.playback.is_some() {
                    return Err(invalid(
                        "Finish/cancel the active recording or leave playback first",
                    ));
                }
                let recorder = Recorder::new(name, &motion::snapshot(ws)?)?;
                motion::state(ws)?.recorder = Some(recorder);
            }
            Command::FinishRecording => {
                let metadata = saved.unwrap();
                if ws
                    .stored
                    .recordings
                    .values()
                    .map(|m| m.bytes)
                    .sum::<usize>()
                    + metadata.bytes
                    > TOTAL_BYTES
                {
                    return Err(invalid(
                        "Workspace recordings exceed 64 MiB; delete an older recording",
                    ));
                }
                let mut next = ws.stored.clone();
                next.recordings.insert(metadata.id.clone(), metadata);
                self.persist(&next).await?;
                ws.stored = next;
                motion::state(ws)?.recorder = None;
            }
            Command::CancelRecording => {
                motion::state(ws)?
                    .recorder
                    .take()
                    .ok_or_else(|| invalid("No recording is active"))?;
            }
            Command::Select { frame_index, .. } => {
                let (metadata, record) = loaded.unwrap();
                let mechanism = &motion::state(ws)?.mechanism;
                let sample = record
                    .samples
                    .get(frame_index)
                    .ok_or_else(|| invalid("Recorded frame index out of range"))?;
                sample.evaluation(mechanism)?;
                frame_times_s = Some(record.samples.iter().map(|s| s.time_s).collect());
                motion::state(ws)?.playback = Some(Playback {
                    metadata,
                    recording: record,
                    index: frame_index,
                });
            }
            Command::Leave => {
                motion::state(ws)?
                    .playback
                    .take()
                    .ok_or_else(|| invalid("Playback is not active"))?;
            }
            Command::Delete { recording_id } => {
                if motion::state(ws)?
                    .playback
                    .as_ref()
                    .is_some_and(|p| p.metadata.id == recording_id)
                {
                    return Err(invalid(
                        "Leave playback before deleting the selected recording",
                    ));
                }
                let mut next = ws.stored.clone();
                next.recordings
                    .remove(&recording_id)
                    .ok_or_else(|| invalid("Unknown recording in this workspace"))?;
                next.recording_imports.remove(&recording_id);
                self.persist(&next).await?;
                ws.stored = next;
            }
        }
        // List is read-only. Every mutation invalidates in-flight pose operations.
        if mutating {
            motion::state(ws)?.version = next_version;
        }
        let mut snapshot = motion::snapshot(ws)?;
        snapshot.actor = Some(actor.into());
        let recordings = ws.stored.recordings.values().cloned().collect();
        let _ = self.motion_events.send(snapshot.clone());
        if recordings_changed {
            let _ = self.activity.send(crate::events::Activity {
                workspace_id: args.workspace_id,
                kind: "recordings_changed".into(),
                job: None,
                actor: Some(actor.into()),
                tool: Some("control_playback".into()),
            });
        }
        Ok(Reply {
            snapshot,
            recordings,
            frame_times_s,
        })
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[test]
    fn capture_retention_is_bounded_and_full_status_does_not_change_saved_samples() {
        let evaluation = Evaluation {
            poses: BTreeMap::from([(
                "part".into(),
                Pose {
                    position: [0.; 3],
                    quaternion: [0., 0., 0., 1.],
                },
            )]),
            joints: vec![],
            constraints: None,
        };
        let mut recorder = Recorder {
            name: "Bounded".into(),
            mode: "simulate".into(),
            started: Instant::now(),
            initial_sim_time: Some(10.),
            capacity: 3,
            frames: vec![Arc::new(Sample::capture(0., 0, evaluation.clone()))],
            full: false,
        };
        for i in 1..100 {
            recorder.capture(evaluation.clone(), i, Some(10. + i as f64 / 120.));
        }
        assert_eq!(recorder.frames.len(), 3);
        assert!(recorder.status().capacity_reached);
        assert_eq!(recorder.frames[2].pose_version, 2);
        assert!((recorder.status().duration_s - 2. / 120.).abs() < 1e-12);
    }
}

#[cfg(test)]
mod joint_identity_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn recorded_coordinates_origins_and_axes_keep_joint_identity_with_unsorted_definitions() {
        let definition=crate::kinematics::decode(&json!({"joints":[{"id":"child","parent":"a","child":"b","parent_frame":{"position_m":[0.03,0,0]},"kind":{"type":"prismatic","axis":[0,1,0]},"initial":0.02,"limits":[0,0.1]},{"id":"parent","parent":null,"child":"a","kind":{"type":"prismatic","axis":[1,0,0]},"initial":0.01,"limits":[0,0.1]}]})).unwrap();
        let mechanism=Mechanism::new(definition,&json!([{"instance_id":"a","position":[0,0,0]},{"instance_id":"b","position":[30,0,0]}])).unwrap();
        let mut recorded = mechanism.evaluate().unwrap();
        for state in &mut recorded.joints {
            state.coordinate = if state.id == "parent" { 0.04 } else { 0.07 };
        }
        recorded.poses.get_mut("a").unwrap().position = [40., 0., 0.];
        recorded.poses.get_mut("b").unwrap().position = [70., 70., 0.];
        let sample = Sample::capture(0., 5, recorded.clone());
        let replay = sample.evaluation(&mechanism).unwrap();
        assert_eq!(
            serde_json::to_value(&replay.poses).unwrap(),
            serde_json::to_value(&recorded.poses).unwrap()
        );
        for state in replay.joints {
            assert!(!state.drivable);
            if state.id == "parent" {
                assert_eq!(state.coordinate, 0.04);
                assert_eq!(state.axis_world, [1., 0., 0.]);
                assert_eq!(state.origin_mm, [0., 0., 0.]);
            } else {
                assert_eq!(state.coordinate, 0.07);
                assert_eq!(state.axis_world, [0., 1., 0.]);
                assert!((state.origin_mm[0] - 70.).abs() < 1e-12);
            }
        }
    }
}
