//! Heuristic printability pre-flight checks on STL triangle soup.
//! Runs entirely in core on exported/managed meshes — no slicer, no GPU.
//! Results are estimates for triage, not a substitute for slicing.
use crate::{Error, Result, mesh_ops};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub name: &'static str,
    /// Generic build volume in mm; oversized axes are flagged.
    pub build_volume_mm: [f64; 3],
    /// Shortest acceptable mesh edge, mm.
    pub min_feature_mm: f64,
    /// Downward-facing surface steeper than this (deg from horizontal) counts as overhang.
    pub overhang_deg: f64,
    /// Overhang area fraction that triggers a warning.
    pub overhang_warn_fraction: f64,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "fdm",
        build_volume_mm: [256.0, 256.0, 256.0],
        min_feature_mm: 0.8,
        overhang_deg: 45.0,
        overhang_warn_fraction: 0.15,
    },
    Preset {
        name: "resin",
        build_volume_mm: [200.0, 200.0, 200.0],
        min_feature_mm: 0.3,
        overhang_deg: 30.0,
        overhang_warn_fraction: 0.25,
    },
    Preset {
        name: "sla",
        build_volume_mm: [250.0, 250.0, 250.0],
        min_feature_mm: 0.3,
        overhang_deg: 30.0,
        overhang_warn_fraction: 0.25,
    },
    Preset {
        name: "generic",
        build_volume_mm: [300.0, 300.0, 300.0],
        min_feature_mm: 0.4,
        overhang_deg: 60.0,
        overhang_warn_fraction: 0.3,
    },
];

pub fn preset(name: &str) -> Result<Preset> {
    let name = name.trim().to_ascii_lowercase();
    PRESETS
        .iter()
        .copied()
        .find(|preset| preset.name == name)
        .ok_or_else(|| {
            Error::domain(
                "INVALID_REQUEST",
                format!("Unknown process {name:?}; want fdm, resin, sla or generic"),
            )
        })
}

#[derive(Clone, Serialize, JsonSchema)]
pub struct Warning {
    pub code: String,
    pub severity: String,
    pub message: String,
}

#[derive(Clone, Serialize, JsonSchema)]
pub struct Bounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub size: [f64; 3],
}

#[derive(Clone, Serialize, JsonSchema)]
pub struct PrintabilityReport {
    pub process: String,
    pub source: String,
    pub triangles: usize,
    pub watertight: bool,
    pub boundary_edges: usize,
    pub nonmanifold_edges: usize,
    pub degenerate_triangles: usize,
    pub bounds_mm: Bounds,
    pub exceeds_build_volume: Vec<String>,
    pub min_edge_mm: f64,
    pub min_triangle_area_mm2: f64,
    pub small_feature_triangles: usize,
    pub overhang_area_mm2: f64,
    pub overhang_area_fraction: f64,
    pub warnings: Vec<Warning>,
    pub approximate: bool,
}

fn vertex_key(point: [f64; 3]) -> [u32; 3] {
    point.map(|coordinate| {
        let value = coordinate as f32;
        if value == 0.0 {
            0
        } else {
            value.to_bits()
        }
    })
}

fn warn(warnings: &mut Vec<Warning>, code: &str, severity: &str, message: String) {
    warnings.push(Warning {
        code: code.into(),
        severity: severity.into(),
        message,
    });
}

/// Run all checks on an STL blob. `source` labels the geometry origin in the report.
pub fn analyze(bytes: &[u8], preset: Preset, source: &str) -> Result<PrintabilityReport> {
    let triangles = mesh_ops::read_triangles(bytes)?;
    let mut warnings = Vec::new();
    let mut edge_use: HashMap<([u32; 3], [u32; 3]), u32> = HashMap::new();
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    let mut min_edge = f64::INFINITY;
    let mut min_area = f64::INFINITY;
    let mut degenerate = 0usize;
    let mut small_features = 0usize;
    let mut total_area = 0.0f64;
    let mut overhang_area = 0.0f64;
    let overhang_cos = preset.overhang_deg.to_radians().cos();

    for triangle in &triangles {
        let [a, b, c] = *triangle;
        for vertex in [a, b, c] {
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex[axis]);
                max[axis] = max[axis].max(vertex[axis]);
            }
        }
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let bc = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
        for edge in [
            (a, b, (ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2]).sqrt()),
            (b, c, (bc[0] * bc[0] + bc[1] * bc[1] + bc[2] * bc[2]).sqrt()),
            (c, a, (ac[0] * ac[0] + ac[1] * ac[1] + ac[2] * ac[2]).sqrt()),
        ] {
            min_edge = min_edge.min(edge.2);
        }
        let normal = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let twice_area = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if twice_area == 0.0 {
            degenerate += 1;
            continue;
        }
        let area = twice_area / 2.0;
        min_area = min_area.min(area);
        total_area += area;
        let nz = normal[2] / twice_area;
        if nz < -overhang_cos {
            overhang_area += area;
        }
        if min_edge < preset.min_feature_mm {
            small_features += 1;
        }
        let keys = [vertex_key(a), vertex_key(b), vertex_key(c)];
        for pair in [(keys[0], keys[1]), (keys[1], keys[2]), (keys[2], keys[0])] {
            let edge = if pair.0 <= pair.1 { pair } else { (pair.1, pair.0) };
            *edge_use.entry(edge).or_insert(0) += 1;
        }
    }

    let mut boundary = 0usize;
    let mut nonmanifold = 0usize;
    for count in edge_use.values() {
        match *count {
            1 => boundary += 1,
            2 => {}
            _ => nonmanifold += 1,
        }
    }
    let watertight = boundary == 0 && nonmanifold == 0;

    let size = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let mut exceeds = Vec::new();
    for (axis, label) in ["x", "y", "z"].iter().enumerate() {
        if size[axis] > preset.build_volume_mm[axis] {
            exceeds.push(format!("{label} {:.1} mm > {:.0} mm", size[axis], preset.build_volume_mm[axis]));
        }
    }
    let overhang_fraction = if total_area > 0.0 { overhang_area / total_area } else { 0.0 };

    if !watertight {
        warn(
            &mut warnings,
            "not_watertight",
            "error",
            format!(
                "Mesh is not closed: {boundary} boundary edges, {nonmanifold} non-manifold edges. Most slicers require a watertight solid.",
            ),
        );
    }
    if degenerate > 0 {
        warn(
            &mut warnings,
            "degenerate_triangles",
            "warning",
            format!("{degenerate} zero-area triangles may confuse slicers."),
        );
    }
    if !exceeds.is_empty() {
        warn(
            &mut warnings,
            "exceeds_build_volume",
            "error",
            format!("Part exceeds the {} build volume on {}.", preset.name, exceeds.join(", ")),
        );
    }
    if small_features > 0 {
        warn(
            &mut warnings,
            "small_features",
            "warning",
            format!(
                "{small_features} triangles sit on edges under {:.2} mm — likely below {} resolution.",
                preset.min_feature_mm, preset.name
            ),
        );
    }
    if overhang_fraction > preset.overhang_warn_fraction {
        warn(
            &mut warnings,
            "heavy_overhang",
            "warning",
            format!(
                "{:.0}% of surface area faces downward steeper than {}° — expect supports or print issues.",
                overhang_fraction * 100.0, preset.overhang_deg
            ),
        );
    }
    if triangles.is_empty() {
        warn(&mut warnings, "empty", "error", "Mesh has no triangles.".into());
    }

    Ok(PrintabilityReport {
        process: preset.name.into(),
        source: source.into(),
        triangles: triangles.len(),
        watertight,
        boundary_edges: boundary,
        nonmanifold_edges: nonmanifold,
        degenerate_triangles: degenerate,
        bounds_mm: Bounds {
            min,
            max,
            size,
        },
        exceeds_build_volume: exceeds,
        min_edge_mm: if min_edge.is_finite() { min_edge } else { 0.0 },
        min_triangle_area_mm2: if min_area.is_finite() { min_area } else { 0.0 },
        small_feature_triangles: small_features,
        overhang_area_mm2: overhang_area,
        overhang_area_fraction: overhang_fraction,
        warnings,
        approximate: true,
    })
}

impl crate::Core {
    /// Export the committed revision to STL (mesh projects copy the asset;
    /// CAD projects tessellate through the worker) and run printability checks.
    pub async fn check_printability(
        &self,
        actor: &str,
        workspace_id: &str,
        revision: &str,
        pose_version: Option<u64>,
        process: Option<String>,
    ) -> Result<PrintabilityReport> {
        let preset = preset(process.as_deref().unwrap_or("fdm"))?;
        let _memory = self.pin_workspace(workspace_id).await?;
        self.ensure_actor_active(actor)?;
        {
            let workspaces = self.workspace(workspace_id).await?;
            let current = workspaces[workspace_id].stored.current_revision();
            if current.id != revision {
                return Err(Error::domain(
                    "STALE_ENTITY",
                    "Printability checks require the current revision",
                ));
            }
        }
        let mut export_args = json!({"format": "stl"});
        if let Some(pose) = pose_version {
            export_args["pose_version"] = json!(pose);
        }
        let reply: crate::artifacts::ExportReply = serde_json::from_value(
            self.query_as_actor(actor, workspace_id, revision, "export", export_args)
                .await?,
        )?;
        let bytes = self.artifact(workspace_id, revision, &reply.handle).await?;
        blocking(move || analyze(&bytes, preset, "exported STL"))
            .await
    }
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| Error::domain("IO_ERROR", error.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh_ops::write_stl_binary;

    fn preset() -> Preset {
        PRESETS[0]
    }

    fn report(triangles: &[[[f64; 3]; 3]]) -> PrintabilityReport {
        let bytes = write_stl_binary(triangles).unwrap();
        analyze(&bytes, preset(), "test").unwrap()
    }

    #[test]
    fn closed_tetrahedron_is_watertight() {
        // Unit tetrahedron: every edge shared by exactly two triangles.
        let v = [
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 10.0],
        ];
        let triangles = [
            [v[0], v[2], v[1]],
            [v[0], v[1], v[3]],
            [v[0], v[3], v[2]],
            [v[1], v[2], v[3]],
        ];
        let report = report(&triangles);
        assert!(report.watertight, "{:?}", report.warnings);
        assert_eq!(report.boundary_edges, 0);
        assert_eq!(report.nonmanifold_edges, 0);
        assert!(report.warnings.iter().all(|w| w.code != "not_watertight"));
    }

    #[test]
    fn open_surface_reports_boundary_edges() {
        let triangles = [
            [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
            [[10.0, 0.0, 0.0], [10.0, 10.0, 0.0], [0.0, 10.0, 0.0]],
        ];
        let report = report(&triangles);
        assert!(!report.watertight);
        assert_eq!(report.boundary_edges, 4);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == "not_watertight" && w.severity == "error"));
    }

    #[test]
    fn oversized_part_flags_build_volume() {
        let triangles = [
            [[0.0, 0.0, 0.0], [400.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
            [[400.0, 0.0, 0.0], [400.0, 10.0, 0.0], [0.0, 10.0, 0.0]],
        ];
        let report = report(&triangles);
        assert_eq!(report.exceeds_build_volume.len(), 1);
        assert!(report.exceeds_build_volume[0].starts_with('x'));
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == "exceeds_build_volume"));
    }

    #[test]
    fn downward_facing_triangles_count_as_overhang() {
        // Two triangles: one facing +Z (roof), one facing -Z (overhang).
        let triangles = [
            [[0.0, 0.0, 0.0], [10.0, 0.0, 10.0], [0.0, 10.0, 10.0]],
            [[0.0, 0.0, 0.0], [0.0, 10.0, 10.0], [10.0, 0.0, 10.0]],
        ];
        let report = report(&triangles);
        assert!(report.overhang_area_mm2 > 0.0);
        assert!(report.overhang_area_fraction > 0.0);
    }
}
