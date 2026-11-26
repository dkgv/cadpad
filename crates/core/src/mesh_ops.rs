//! Rigid transforms on managed STL mesh assets. Pure Rust — mesh-only projects
//! never spawn the Python worker, so edits run entirely in core.
use crate::{Core, Error, Job, Result, portable};
use glam::{DMat4, DQuat, DVec3};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MirrorAxis {
    X,
    Y,
    Z,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AxisAngle {
    /// Rotation axis in mesh coordinates; need not be unit length.
    pub axis: [f64; 3],
    /// Rotation angle in degrees.
    pub angle_deg: f64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransformMesh {
    pub workspace_id: String,
    /// Current committed revision the draft is based on.
    pub expected_base_revision: String,
    /// Continue an existing draft instead of starting a new one.
    pub draft_id: Option<String>,
    /// Managed asset key as listed by get_project (the `assets` map). Must be STL.
    pub asset: String,
    /// Translation in millimetres, applied after scale and rotation.
    pub translate: Option<[f64; 3]>,
    /// Rotation about an axis through the origin, applied after scale.
    pub rotate: Option<AxisAngle>,
    /// Per-axis scale factors. Negative components mirror and flip winding.
    pub scale: Option<[f64; 3]>,
    pub mirror_axis: Option<MirrorAxis>,
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| Error::domain("IO_ERROR", error.to_string()))?
}

fn finite3(value: [f64; 3], label: &str) -> Result<[f64; 3]> {
    if value.iter().any(|v| !v.is_finite()) {
        return Err(Error::domain(
            "INVALID_REQUEST",
            format!("{label} components must be finite"),
        ));
    }
    Ok(value)
}

/// Compose the requested ops into one matrix plus a winding-flip flag.
/// Order: scale (with mirror folded in) → rotate → translate.
fn compose(args: &TransformMesh) -> Result<(DMat4, bool)> {
    let mut scale = args.scale.unwrap_or([1.0, 1.0, 1.0]);
    finite3(scale, "scale")?;
    if scale.iter().any(|v| *v == 0.0) {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "scale components must be non-zero",
        ));
    }
    if let Some(axis) = args.mirror_axis {
        match axis {
            MirrorAxis::X => scale[0] = -scale[0],
            MirrorAxis::Y => scale[1] = -scale[1],
            MirrorAxis::Z => scale[2] = -scale[2],
        }
    }
    // An odd number of negative axes inverts triangle winding.
    let flip = scale.iter().filter(|v| **v < 0.0).count() % 2 == 1;
    let scale_abs = DVec3::new(scale[0].abs(), scale[1].abs(), scale[2].abs());

    let rotation = match args.rotate {
        Some(rotate) => {
            let axis = DVec3::from_array(finite3(rotate.axis, "rotate.axis")?);
            if axis.length_squared() == 0.0 || !rotate.angle_deg.is_finite() {
                return Err(Error::domain(
                    "INVALID_REQUEST",
                    "rotate requires a non-zero axis and finite angle_deg",
                ));
            }
            DQuat::from_axis_angle(axis.normalize(), rotate.angle_deg.to_radians())
        }
        None => DQuat::IDENTITY,
    };
    let translate = DVec3::from_array(finite3(
        args.translate.unwrap_or([0.0, 0.0, 0.0]),
        "translate",
    )?);

    if args.translate.is_none()
        && args.rotate.is_none()
        && args.scale.is_none()
        && args.mirror_axis.is_none()
    {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "transform_mesh requires at least one of translate, rotate, scale or mirror_axis",
        ));
    }
    let matrix = DMat4::from_scale_rotation_translation(scale_abs, rotation, translate);
    Ok((matrix, flip))
}

/// Read an STL blob as a triangle soup. Vertices stream in facet order.
pub(crate) fn read_triangles(bytes: &[u8]) -> Result<Vec<[[f64; 3]; 3]>> {
    let mut triangles = Vec::new();
    let mut facet = [[0.0f64; 3]; 3];
    let mut corner = 0usize;
    portable::visit_stl(bytes, |point| {
        facet[corner] = point;
        corner += 1;
        if corner == 3 {
            corner = 0;
            triangles.push(facet);
        }
        Ok(())
    })?;
    if triangles.is_empty() {
        return Err(Error::domain("INVALID_GEOMETRY", "STL has no triangles"));
    }
    Ok(triangles)
}

/// Binary STL writer: 80-byte header, u32 count, 50-byte facets.
pub(crate) fn write_stl_binary(triangles: &[[[f64; 3]; 3]]) -> Result<Vec<u8>> {
    let count = u32::try_from(triangles.len())
        .map_err(|_| Error::domain("RESOURCE_LIMIT", "Mesh exceeds STL facet count limit"))?;
    let mut out = Vec::with_capacity(84 + triangles.len() * 50);
    let mut header = [b' '; 80];
    header[..11].copy_from_slice(b"Cadpad mesh");
    out.extend_from_slice(&header);
    out.extend_from_slice(&count.to_le_bytes());
    for triangle in triangles {
        let [a, b, c] = *triangle;
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let normal = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        let normal = if length > 0.0 {
            [normal[0] / length, normal[1] / length, normal[2] / length]
        } else {
            [0.0, 0.0, 0.0]
        };
        for value in normal {
            out.extend_from_slice(&(value as f32).to_le_bytes());
        }
        for vertex in triangle {
            for coordinate in vertex {
                let value = *coordinate as f32;
                if !value.is_finite() {
                    return Err(Error::domain(
                        "INVALID_GEOMETRY",
                        "Transform produced non-finite coordinates",
                    ));
                }
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    Ok(out)
}

/// Resolve a caller-supplied asset reference to a managed `inputs.assets` key.
/// Accepts the bare key, an `assets/`-prefixed path, or the stripped form.
fn resolve_asset_key<'a>(assets: &'a BTreeMap<String, String>, requested: &str) -> Result<&'a str> {
    if requested.is_empty() || requested.len() > 512 {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "asset must be 1–512 characters",
        ));
    }
    let stripped = requested.strip_prefix("assets/").unwrap_or(requested);
    let prefixed = format!("assets/{stripped}");
    let mut matches: Vec<&'a str> = Vec::new();
    for candidate in [requested, stripped, prefixed.as_str()] {
        if let Some((key, _)) = assets.get_key_value(candidate) {
            if !matches.contains(&key.as_str()) {
                matches.push(key.as_str());
            }
        }
    }
    match matches.as_slice() {
        [key] => Ok(key),
        [] => Err(Error::domain(
            "INVALID_REQUEST",
            format!(
                "Unknown asset {requested:?}; STL assets: {}",
                available_stl(assets)
            ),
        )),
        _ => Err(Error::domain(
            "INVALID_REQUEST",
            format!("Asset reference {requested:?} is ambiguous"),
        )),
    }
}

fn available_stl(assets: &BTreeMap<String, String>) -> String {
    let names: Vec<&str> = assets
        .keys()
        .filter(|key| key.to_ascii_lowercase().ends_with(".stl"))
        .map(String::as_str)
        .collect();
    if names.is_empty() {
        "(none)".into()
    } else {
        names.join(", ")
    }
}

impl Core {
    /// Stage an STL asset transform and queue a rebuild. Returns the build Job.
    pub async fn transform_mesh(&self, actor: &str, args: TransformMesh) -> Result<Job> {
        if args.asset.len() > 512 || !args.asset.to_ascii_lowercase().ends_with(".stl") {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "transform_mesh only applies to .stl assets; edit CadQuery source for BRep parts",
            ));
        }
        let _memory = self.pin_workspace(&args.workspace_id).await?;
        let _publication = self.blob_publication.read().await;
        let (matrix, flip) = compose(&args)?;
        let (key, digest) = {
            let workspaces = self.workspace(&args.workspace_id).await?;
            self.ensure_actor_active(actor)?;
            let ws = workspaces
                .get(&args.workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != args.expected_base_revision {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Expected base is no longer current",
                ));
            }
            let current = ws.stored.current_revision();
            let key = resolve_asset_key(&current.inputs.assets, &args.asset)?.to_string();
            let source_digest = current.inputs.assets[&key].clone();
            if !portable::valid_hash(&source_digest) {
                return Err(Error::domain("INVALID_REQUEST", "Invalid asset digest"));
            }
            let blobs = self.root.join("blobs");
            let blob = portable::read_bounded(&blobs.join(&source_digest), portable::ASSET_LIMIT)?;
            if portable::bytes_hash(&blob) != source_digest {
                return Err(Error::domain(
                    "INVALID_GEOMETRY",
                    "Managed asset digest mismatch",
                ));
            }
            let triangles = blocking(move || read_triangles(&blob)).await?;
            let transformed = blocking(move || {
                let mut out = Vec::with_capacity(triangles.len());
                for mut triangle in triangles {
                    if flip {
                        triangle.swap(1, 2);
                    }
                    for vertex in &mut triangle {
                        *vertex = matrix
                            .transform_point3(DVec3::from_array(*vertex))
                            .to_array();
                    }
                    out.push(triangle);
                }
                write_stl_binary(&out)
            })
            .await?;
            let digest = portable::bytes_hash(&transformed);
            self.publish_asset_blob(digest.clone(), transformed).await?;
            (key, digest)
        };
        // Stage the new digest under the same asset key, then build.
        let draft = {
            let mut workspaces = self.workspace(&args.workspace_id).await?;
            let ws = workspaces
                .get_mut(&args.workspace_id)
                .ok_or_else(|| Error::domain("APP_UNAVAILABLE", "Unknown workspace"))?;
            if ws.stored.current != args.expected_base_revision {
                return Err(Error::domain(
                    "REVISION_CONFLICT",
                    "Expected base is no longer current",
                ));
            }
            let mut draft = if let Some(draft_id) = args.draft_id.as_deref() {
                let draft = ws.stored.drafts.load(draft_id).await?;
                if draft.actor != actor || draft.base_revision != args.expected_base_revision {
                    return Err(Error::domain(
                        "REVISION_CONFLICT",
                        "Draft ownership or base mismatch",
                    ));
                }
                draft.as_ref().clone()
            } else {
                Self::input_draft(
                    ws.stored.current_revision().inputs.clone(),
                    &args.expected_base_revision,
                    actor,
                )
            };
            draft.restore_revision = None;
            draft.inputs.assets.insert(key, digest);
            self.validate_asset_budget(&draft.inputs).await?;
            portable::validate_inputs(&draft.inputs)?;
            let mut next = ws.stored.clone();
            next.drafts.insert(draft.id.clone(), draft.clone());
            self.persist(&next).await?;
            ws.stored = next;
            draft
        };
        self.build(
            &args.workspace_id,
            &args.expected_base_revision,
            &draft.id,
            actor,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> TransformMesh {
        TransformMesh {
            workspace_id: String::new(),
            expected_base_revision: String::new(),
            draft_id: None,
            asset: "assets/part.stl".into(),
            translate: None,
            rotate: None,
            scale: None,
            mirror_axis: None,
        }
    }

    #[test]
    fn compose_requires_an_operation() {
        assert!(compose(&args()).is_err());
    }

    #[test]
    fn compose_rejects_zero_scale() {
        let mut a = args();
        a.scale = Some([1.0, 0.0, 1.0]);
        assert!(compose(&a).is_err());
    }

    #[test]
    fn mirror_axis_flips_winding_on_single_axis() {
        let mut a = args();
        a.mirror_axis = Some(MirrorAxis::X);
        let (_, flip) = compose(&a).unwrap();
        assert!(flip);
        let mut b = args();
        b.scale = Some([-1.0, -1.0, 1.0]);
        let (_, flip) = compose(&b).unwrap();
        assert!(!flip, "two negative axes preserve winding");
    }

    #[test]
    fn stl_roundtrip_preserves_geometry() {
        let triangles = [
            [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
            [[10.0, 0.0, 0.0], [10.0, 10.0, 0.0], [0.0, 10.0, 0.0]],
        ];
        let bytes = write_stl_binary(&triangles).unwrap();
        let back = read_triangles(&bytes).unwrap();
        assert_eq!(back.len(), 2);
        for (want, got) in triangles.iter().zip(back.iter()) {
            for (w, g) in want.iter().zip(got.iter()) {
                for axis in 0..3 {
                    assert!((w[axis] - g[axis]).abs() < 1e-4);
                }
            }
        }
    }

    #[test]
    fn transform_scales_and_translates() {
        let triangles = [[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]];
        let bytes = write_stl_binary(&triangles).unwrap();
        let read = read_triangles(&bytes).unwrap();
        let (matrix, flip) = {
            let mut a = args();
            a.scale = Some([2.0, 2.0, 2.0]);
            a.translate = Some([10.0, 0.0, 0.0]);
            compose(&a).unwrap()
        };
        assert!(!flip);
        let out: Vec<[[f64; 3]; 3]> = read
            .iter()
            .map(|t| t.map(|v| matrix.transform_point3(DVec3::from_array(v)).to_array()))
            .collect();
        assert_eq!(out[0][1], [12.0, 0.0, 0.0]);
    }
}
