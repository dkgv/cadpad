//! Selection and viewport requests are workspace/revision scoped. Rendering stays
//! in the frontend; a missing or mismatched viewport is an explicit tool error.
use crate::{Core, Error, Result, tools::Entity};
use base64::Engine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, Serialize, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub position: [f64; 3],
    pub quaternion: [f64; 4],
    pub target: [f64; 3],
    pub zoom: f64,
    pub orthographic: bool,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Iso,
    Front,
    Rear,
    Top,
    Bottom,
    Left,
    Right,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub workspace_id: String,
    pub revision: String,
    pub pose_version: Option<u64>,
    pub camera_preset: Option<Preset>,
    pub camera: Option<Camera>,
    pub dimensions: Option<bool>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub enum ImageMime {
    #[serde(rename = "image/png")]
    Png,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CaptureImage {
    pub mime_type: ImageMime,
    pub data: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CapturePart {
    pub instance_id: String,
    pub bounds: crate::geometry::Bounds,
    pub volume: Option<f64>,
    pub coordinate_space: crate::geometry::CoordinateSpace,
}
/// MCP keeps the PNG in a content block; all numeric metadata stays structured.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CaptureMetadata {
    #[serde(flatten)]
    pub context: crate::geometry::ExactContext,
    pub camera: Camera,
    pub poses: std::collections::BTreeMap<String, crate::kinematics::Pose>,
    pub joint_states: Vec<crate::kinematics::JointState>,
    pub visible_part_ids: Vec<String>,
    #[schemars(range(min = 1, max = 1600))]
    pub width: u32,
    #[schemars(range(min = 1, max = 1200))]
    pub height: u32,
    #[schemars(length(max = 128))]
    pub numeric_context: Vec<CapturePart>,
}
/// Native dispatch always includes the image, including for the built-in agent.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct CaptureReply {
    #[serde(flatten)]
    pub metadata: CaptureMetadata,
    pub image: CaptureImage,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DimensionScope {
    Selected,
    Pinned,
    All,
}
#[derive(Clone, Serialize, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct SetDimensions {
    pub workspace_id: String,
    pub visible: bool,
    pub scope: DimensionScope,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DimensionsReply {
    pub workspace_id: String,
    pub revision: String,
    pub visible: bool,
    pub scope: DimensionScope,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ViewRequest {
    pub id: String,
    pub workspace_id: String,
    pub revision: String,
    pub command: String,
    pub arguments: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewReply {
    pub data: Option<Value>,
    pub error: Option<Value>,
}
struct Pending {
    token: String,
    sender: oneshot::Sender<ViewReply>,
}
#[derive(Default)]
struct ViewState {
    sender: Option<(String, mpsc::Sender<ViewRequest>)>,
    pending: HashMap<String, Pending>,
    selections: HashMap<String, Vec<Entity>>,
}
#[derive(Clone, Default)]
pub struct ViewService {
    state: Arc<Mutex<ViewState>>,
}
struct PendingGuard {
    id: String,
    state: Arc<Mutex<ViewState>>,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.state.lock().unwrap().pending.remove(&self.id);
    }
}
impl ViewService {
    pub fn attach(&self) -> (String, mpsc::Receiver<ViewRequest>) {
        let token = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = mpsc::channel(8);
        let mut state = self.state.lock().unwrap();
        state.pending.clear();
        state.sender = Some((token.clone(), sender));
        (token, receiver)
    }
    pub fn complete(&self, token: &str, id: &str, reply: ViewReply) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state
            .pending
            .get(id)
            .is_none_or(|pending| pending.token != token)
        {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Viewport response belongs to an expired request or window",
            ));
        }
        let pending = state.pending.remove(id).unwrap();
        let _ = pending.sender.send(reply);
        Ok(())
    }
    pub async fn request(
        &self,
        workspace: &str,
        revision: &str,
        command: &str,
        arguments: Value,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let receive = {
            let mut state = self.state.lock().unwrap();
            let (token, sender) = state.sender.clone().ok_or_else(|| {
                Error::domain(
                    "APP_UNAVAILABLE",
                    "No CAD viewport is connected; open the workspace in Cadpad",
                )
            })?;
            let (send, receive) = oneshot::channel();
            state.pending.insert(
                id.clone(),
                Pending {
                    token,
                    sender: send,
                },
            );
            if sender
                .try_send(ViewRequest {
                    id: id.clone(),
                    workspace_id: workspace.into(),
                    revision: revision.into(),
                    command: command.into(),
                    arguments,
                })
                .is_err()
            {
                state.pending.remove(&id);
                return Err(Error::domain(
                    "APP_UNAVAILABLE",
                    "Viewport is unavailable or busy; retry after opening the workspace",
                ));
            }
            receive
        };
        let _guard = PendingGuard {
            id,
            state: self.state.clone(),
        };
        if *cancel.borrow() {
            return Err(Error::domain("CANCELLED", "Viewport request cancelled"));
        }
        let reply = tokio::select! {
            reply=tokio::time::timeout(Duration::from_secs(10),receive)=>reply.map_err(|_|Error::domain("APP_UNAVAILABLE","Viewport did not respond within 10 seconds"))?.map_err(|_|Error::domain("APP_UNAVAILABLE","Viewport disconnected"))?,
            _=cancel.changed()=>return Err(Error::domain("CANCELLED","Viewport request cancelled")),
        };
        if let Some(error) = reply.error {
            return Err(Error::Domain {
                code: error["code"].as_str().unwrap_or("APP_UNAVAILABLE").into(),
                message: error["message"]
                    .as_str()
                    .unwrap_or("Viewport operation failed")
                    .into(),
                details: Value::Null,
            });
        }
        reply
            .data
            .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Empty viewport response"))
    }
}
fn validate_camera(camera: &Camera) -> Result<()> {
    let finite = camera
        .position
        .iter()
        .chain(camera.quaternion.iter())
        .chain(camera.target.iter())
        .all(|value| value.is_finite());
    if !finite
        || !camera.zoom.is_finite()
        || camera.zoom <= 0.0
        || (camera.quaternion.iter().map(|x| x * x).sum::<f64>() - 1.0).abs() > 1e-6
    {
        return Err(Error::domain(
            "INVALID_PARAMETER",
            "Camera requires finite coordinates, positive zoom and a normalized xyzw quaternion",
        ));
    }
    Ok(())
}
impl Core {
    pub fn attach_view(&self) -> (String, mpsc::Receiver<ViewRequest>) {
        self.views.attach()
    }
    pub fn complete_view(&self, token: &str, id: &str, reply: ViewReply) -> Result<()> {
        self.views.complete(token, id, reply)
    }
    pub async fn set_selection(
        &self,
        workspace: &str,
        revision: &str,
        entities: Vec<Entity>,
    ) -> Result<()> {
        let project = self.get_project(workspace).await?;
        if project["revision"] != revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Selection revision is no longer committed",
            ));
        }
        if entities.len() > 32 {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Select at most 32 entities",
            ));
        }
        for entity in &entities {
            let part = project["model"]["parts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|part| part["instance_id"] == entity.instance_id)
                .ok_or_else(|| Error::domain("STALE_ENTITY", "Unknown selected instance"))?;
            let (kind, index) = entity
                .entity_id
                .split_once(':')
                .ok_or_else(|| Error::domain("STALE_ENTITY", "Invalid selected entity"))?;
            let index = index
                .parse::<u64>()
                .map_err(|_| Error::domain("STALE_ENTITY", "Invalid selected topology index"))?;
            let valid = match kind {
                "part" => index == 0,
                "face" => index < part["face_count"].as_u64().unwrap_or(0),
                "edge" => index < part["edge_count"].as_u64().unwrap_or(0),
                _ => false,
            };
            if entity.revision != revision || !valid {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Selection does not belong to this revision",
                ));
            }
        }
        self.views
            .state
            .lock()
            .unwrap()
            .selections
            .insert(workspace.into(), entities);
        Ok(())
    }
    pub async fn get_selection(&self, workspace: &str) -> Result<Value> {
        let project = self.get_project(workspace).await?;
        let entities = self
            .views
            .state
            .lock()
            .unwrap()
            .selections
            .get(workspace)
            .cloned()
            .unwrap_or_default();
        let stale = entities
            .iter()
            .any(|entity| project["revision"] != entity.revision);
        let entities = if stale { vec![] } else { entities };
        let parts = project["model"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|part| {
                entities
                    .iter()
                    .any(|entity| part["instance_id"] == entity.instance_id)
            })
            .map(|part| {
                serde_json::from_value::<crate::geometry::SelectedPart>(json!({
                    "instance_id":part["instance_id"],"source":part["source"],"bounds":part["bounds"],
                    "volume":part["volume"],"features":part["features"],"units":"mm"
                }))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!(crate::geometry::SelectionReply {
            workspace_id: workspace.into(),
            revision: project["revision"].as_str().unwrap().into(),
            entities,
            parts,
            expired: stale,
            pose_version: project["motion"]["pose_version"].as_u64().unwrap(),
            joint_states: serde_json::from_value(project["motion"]["joints"].clone())?,
            units: crate::geometry::Units::Millimetres,
        }))
    }
    pub async fn capture_view(&self, actor: &str, mut args: Capture) -> Result<Value> {
        self.ensure_actor_active(actor)?;
        let width = args.width.unwrap_or(1024);
        let height = args.height.unwrap_or(768);
        if width == 0 || height == 0 || width > 1600 || height > 1200 {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "Capture dimensions must be between 1×1 and 1600×1200",
            ));
        }
        if args.camera.is_some() && args.camera_preset.is_some() {
            return Err(Error::domain(
                "INVALID_PARAMETER",
                "Choose an explicit camera or a preset",
            ));
        }
        if let Some(camera) = &args.camera {
            validate_camera(camera)?;
        }
        let project = self.get_project(&args.workspace_id).await?;
        if project["revision"] != args.revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Capture revision is no longer committed",
            ));
        }
        let motion = self.get_motion(&args.workspace_id, &args.revision).await?;
        if args.pose_version.unwrap_or(0) != motion.pose_version {
            return Err(Error::domain(
                "POSE_CONFLICT",
                "Capture requires the current pose_version from get_motion",
            ));
        }
        args.pose_version = Some(motion.pose_version);
        args.width = Some(width);
        args.height = Some(height);
        let mut arguments = serde_json::to_value(&args)?;
        arguments["motion"] = json!(motion);
        let data = self
            .views
            .request(
                &args.workspace_id,
                &args.revision,
                "capture",
                arguments,
                self.actor_signal(actor),
            )
            .await?;
        let camera: Camera = serde_json::from_value(data["camera"].clone())
            .map_err(|_| Error::domain("INVALID_REQUEST", "Viewport returned an invalid camera"))?;
        validate_camera(&camera)?;
        let encoded = data["image"].as_str().ok_or_else(|| {
            Error::domain("INVALID_REQUEST", "Viewport did not return a PNG image")
        })?;
        if encoded.len() > 6 * 1024 * 1024 {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Viewport image exceeds the capture limit",
            ));
        }
        let png = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| Error::domain("INVALID_REQUEST", "Invalid capture encoding"))?;
        if png.len() < 24
            || png[..8] != [137, 80, 78, 71, 13, 10, 26, 10]
            || &png[12..16] != b"IHDR"
        {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Capture is not a PNG image",
            ));
        }
        let actual_width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let actual_height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        if actual_width == 0 || actual_height == 0 || actual_width > width || actual_height > height
        {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Viewport image dimensions exceed the requested bounds",
            ));
        }
        if data["revision"] != args.revision
            || data["pose_version"] != motion.pose_version
            || self
                .get_motion(&args.workspace_id, &args.revision)
                .await?
                .pose_version
                != motion.pose_version
        {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Capture belongs to an obsolete revision or pose",
            ));
        }
        let visible = data["visible_part_ids"].as_array().ok_or_else(|| {
            Error::domain("INVALID_REQUEST", "Viewport did not report visible parts")
        })?;
        let all_parts = project["model"]["parts"].as_array().unwrap();
        if visible.len() > all_parts.len()
            || visible
                .iter()
                .any(|id| !all_parts.iter().any(|part| part["instance_id"] == *id))
        {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Viewport returned unknown part IDs",
            ));
        }
        let numeric_context = all_parts
            .iter()
            .filter(|part| visible.contains(&part["instance_id"]))
            .take(128)
            .map(|part| {
                serde_json::from_value::<CapturePart>(json!({
                    "instance_id":part["instance_id"],"bounds":part["bounds"],
                    "volume":part["volume"],"coordinate_space":"part_local"
                }))
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!(CaptureReply {
            metadata: CaptureMetadata {
                context: crate::geometry::ExactContext {
                    revision: args.revision,
                    pose_version: motion.pose_version,
                    units: crate::geometry::Units::Millimetres,
                    accuracy: crate::geometry::Accuracy::Exact,
                },
                camera,
                poses: motion.evaluation.poses,
                joint_states: motion.evaluation.joints,
                visible_part_ids: serde_json::from_value(json!(visible))?,
                width: actual_width,
                height: actual_height,
                numeric_context,
            },
            image: CaptureImage {
                mime_type: ImageMime::Png,
                data: encoded.into()
            },
        }))
    }
    pub async fn set_dimensions(&self, actor: &str, args: SetDimensions) -> Result<Value> {
        self.ensure_actor_active(actor)?;
        let project = self.get_project(&args.workspace_id).await?;
        let revision = project["revision"].as_str().unwrap();
        let data = self
            .views
            .request(
                &args.workspace_id,
                revision,
                "dimensions",
                serde_json::to_value(&args)?,
                self.actor_signal(actor),
            )
            .await?;
        let reply: DimensionsReply = serde_json::from_value(data).map_err(|_| {
            Error::domain(
                "INVALID_REQUEST",
                "Viewport returned an invalid dimension acknowledgment",
            )
        })?;
        if reply.workspace_id != args.workspace_id || reply.revision != revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Dimension acknowledgment belongs to another workspace or revision",
            ));
        }
        if reply.visible != args.visible || reply.scope != args.scope {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Viewport did not apply the requested dimension settings",
            ));
        }
        let workspaces = self.workspaces.lock().await;
        if workspaces
            .get(&args.workspace_id)
            .is_none_or(|ws| ws.stored.current != revision)
        {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Dimension acknowledgment belongs to an obsolete revision",
            ));
        }
        Ok(json!(reply))
    }
}
