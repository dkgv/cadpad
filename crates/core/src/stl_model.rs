//! Display and inspect ordinary STL project entries without constructing BRep.
use crate::{Core, Error, Inputs, Result, portable};
use glam::DVec3;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tokio::sync::watch;

pub(crate) const FORMAT: &str = "cadpad-stl-surfaces-v2";
pub(crate) const LEGACY_FORMAT: &str = "cadpad-stl-surfaces-v1";

pub(crate) fn is_mesh(model: &Value) -> bool {
    matches!(
        model["parts"][0]["geometry_fingerprint"]["format"].as_str(),
        Some(FORMAT | LEGACY_FORMAT)
    )
}

/// Receives little-endian vertex triples for batches of preview triangles.
pub type StlPreviewSink = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

pub(crate) struct Source {
    pub digest: String,
    pub bytes: Option<Vec<u8>>,
    pub preview: Option<StlPreviewSink>,
}

impl Source {
    pub(crate) fn stored(digest: String) -> Self {
        Self {
            digest,
            bytes: None,
            preview: None,
        }
    }
}

/// Pack the endpoint bits in lexicographic order so sorting preserves edge IDs.
type EdgeKey = [u64; 3];

fn edge_key(a: [u32; 3], b: [u32; 3]) -> EdgeKey {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    [
        (u64::from(a[0]) << 32) | u64::from(a[1]),
        (u64::from(a[2]) << 32) | u64::from(b[0]),
        (u64::from(b[1]) << 32) | u64::from(b[2]),
    ]
}
type Triangle = [[f32; 3]; 3];

fn points(triangle: Triangle) -> [DVec3; 3] {
    triangle.map(|point| DVec3::from_array(point.map(f64::from)))
}

fn cross(triangle: Triangle) -> DVec3 {
    let [a, b, c] = points(triangle);
    (b - a).cross(c - a)
}

fn vertex_key(point: [f32; 3]) -> [u32; 3] {
    point.map(|coordinate| {
        if coordinate == 0. {
            0
        } else {
            coordinate.to_bits()
        }
    })
}

fn root(parents: &mut [usize], mut index: usize) -> usize {
    while parents[index] != index {
        parents[index] = parents[parents[index]];
        index = parents[index];
    }
    index
}

fn check_cancel(cancel: &watch::Receiver<bool>) -> Result<()> {
    if *cancel.borrow() {
        return Err(Error::domain("CANCELLED", "STL operation cancelled"));
    }
    Ok(())
}

fn read_mesh(blob: &Path, digest: &str) -> Result<Vec<u8>> {
    if !portable::valid_hash(digest) {
        return Err(Error::domain(
            "INVALID_GEOMETRY",
            "Invalid STL asset digest",
        ));
    }
    let bytes = portable::read_bounded(&blob.join(digest), portable::ASSET_LIMIT)?;
    if portable::bytes_hash(&bytes) != digest {
        return Err(Error::domain(
            "INVALID_GEOMETRY",
            "STL asset checksum differs",
        ));
    }
    Ok(bytes)
}

fn topology(
    triangles: &[Triangle],
    normals: &[[f32; 3]],
    with_edges: bool,
    cancel: &watch::Receiver<bool>,
) -> Result<(Vec<usize>, Vec<EdgeKey>)> {
    struct Adjacent {
        first: u32,
        count: u8,
        sharp: bool,
    }
    const CREASE_NORMAL_DOT: f64 = 0.906307787; // cos(25 degrees)
    let mut parents: Vec<_> = (0..triangles.len()).collect();
    // A closed manifold has three facet edges for every two unique edges.
    let edge_capacity = triangles.len() + triangles.len() / 2;
    let mut edges = ahash::AHashMap::<EdgeKey, Adjacent>::with_capacity(edge_capacity);
    for (index, triangle) in triangles.iter().enumerate() {
        if index % 4096 == 0 {
            check_cancel(cancel)?;
        }
        let keys = triangle.map(vertex_key);
        let lhs = DVec3::from_array(normals[index].map(f64::from));
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let edge = edge_key(keys[a], keys[b]);
            let adjacent = edges.entry(edge).or_insert(Adjacent {
                first: index as u32,
                count: 0,
                sharp: false,
            });
            adjacent.count = adjacent.count.saturating_add(1);
            let previous = adjacent.first as usize;
            if previous == index {
                continue;
            }
            // Shared endpoints supply the plane offset. Keep gently curved
            // surfaces tessellated rather than smoothing across their bends.
            let rhs = DVec3::from_array(normals[previous].map(f64::from));
            // Suppress tessellation seams on smooth facets and tolerate
            // inconsistent STL winding along a common plane.
            if with_edges && lhs.dot(rhs).abs() < CREASE_NORMAL_DOT {
                adjacent.sharp = true;
            }
            if lhs.cross(rhs).length_squared() <= 1e-14 {
                let a = root(&mut parents, index);
                let b = root(&mut parents, previous);
                parents[a.max(b)] = a.min(b);
            }
        }
    }
    let surfaces = (0..parents.len())
        .map(|index| root(&mut parents, index))
        .collect();
    let mut boundaries = Vec::new();
    if with_edges {
        boundaries.extend(edges.into_iter().filter_map(|(edge, adjacent)| {
            (adjacent.count != 2 || adjacent.sharp).then_some(edge)
        }));
        boundaries.sort_unstable();
    }
    Ok((surfaces, boundaries))
}

#[derive(Default)]
struct Stats {
    min: Option<DVec3>,
    max: Option<DVec3>,
    area: f64,
    weighted_center: DVec3,
}

impl Stats {
    fn add(&mut self, triangle: Triangle, area: f64) {
        let [a, b, c] = points(triangle);
        let min = a.min(b).min(c);
        let max = a.max(b).max(c);
        self.min = Some(self.min.map_or(min, |old| old.min(min)));
        self.max = Some(self.max.map_or(max, |old| old.max(max)));
        self.area += area;
        self.weighted_center += (a + b + c) * (area / 3.);
    }

    fn bounds(&self) -> Value {
        let (min, max) = (self.min.unwrap(), self.max.unwrap());
        json!({"min":min.to_array(),"max":max.to_array(),"size":(max-min).to_array()})
    }

    fn center(&self) -> [f64; 3] {
        (self.weighted_center / self.area).to_array()
    }
}

fn buffer(cache: &Path, bytes: &[u8]) -> Result<Value> {
    let digest = portable::bytes_hash(bytes);
    let handle = format!("{digest}.bin");
    portable::atomic_write_display_buffer(cache, &handle, bytes)?;
    Ok(json!({"handle":handle,"bytes":bytes.len(),"sha256":digest}))
}

fn publish(
    bytes: &[u8],
    cache: &Path,
    entry: &str,
    digest: &str,
    revision: &str,
    inputs: &Inputs,
    with_edges: bool,
    cancel: &watch::Receiver<bool>,
    preview: Option<&StlPreviewSink>,
) -> Result<Value> {
    let mut triangles = Vec::new();
    let mut normals = Vec::new();
    let mut stats = Stats::default();
    let mut triangle = [[0.; 3]; 3];
    let mut corner = 0;
    let mut seen = 0usize;
    // Bound IPC and preview memory even when the source has millions of facets.
    const FIRST_BATCH: usize = 1024;
    const BATCH: usize = 16384;
    const PREVIEW_LIMIT: usize = FIRST_BATCH + BATCH * 8;
    let mut sent = 0;
    let mut send_preview = |triangles: &[Triangle], finished: bool| {
        let Some(send) = preview else { return };
        let end = triangles.len().min(PREVIEW_LIMIT);
        let batch = if sent == 0 { FIRST_BATCH } else { BATCH };
        if end == sent || (!finished && end - sent < batch) {
            return;
        }
        let mut bytes = Vec::with_capacity((end - sent) * 36);
        for triangle in &triangles[sent..end] {
            for &coordinate in triangle.iter().flatten() {
                bytes.extend_from_slice(&coordinate.to_le_bytes());
            }
        }
        sent = end;
        send(bytes);
    };
    portable::visit_stl(bytes, |point| {
        let point = point.map(|axis| axis as f32);
        if point.iter().any(|axis| !axis.is_finite()) {
            return Err(Error::domain(
                "INVALID_GEOMETRY",
                "STL exceeds the display coordinate range",
            ));
        }
        triangle[corner] = point;
        corner = (corner + 1) % 3;
        if corner == 0 {
            seen += 1;
            if seen % 4096 == 0 {
                check_cancel(cancel)?;
            }
            let cross = cross(triangle);
            let magnitude = cross.length();
            if magnitude > 0. {
                stats.add(triangle, magnitude / 2.);
                triangles.push(triangle);
                send_preview(&triangles, false);
                normals.push((cross / magnitude).to_array().map(|axis| axis as f32));
            }
        }
        Ok(())
    })?;
    if triangles.is_empty() {
        return Err(Error::domain(
            "INVALID_GEOMETRY",
            "STL has no nondegenerate triangles",
        ));
    }
    send_preview(&triangles, true);
    let (surfaces, segments) = topology(&triangles, &normals, with_edges, cancel)?;
    let mut counts = vec![0u32; triangles.len()];
    for &surface in &surfaces {
        counts[surface] += 1;
    }
    let mut offset = 0usize;
    let mut next: Vec<_> = counts
        .iter()
        .map(|&count| {
            let start = offset;
            offset += count as usize;
            start
        })
        .collect();
    let mut order = vec![0; triangles.len()];
    for (index, surface) in surfaces.into_iter().enumerate() {
        order[next[surface]] = index;
        next[surface] += 1;
    }
    counts.retain(|&count| count > 0);

    let mut positions = Vec::with_capacity(order.len() * 36);
    for &index in &order {
        for coordinate in triangles[index].into_iter().flatten() {
            positions.extend_from_slice(&coordinate.to_le_bytes());
        }
    }
    let vertices = buffer(cache, &positions)?;
    drop(positions);
    let mut normal_bytes = Vec::with_capacity(order.len() * 36);
    for &index in &order {
        let normal = normals[index].map(f32::to_le_bytes);
        for _ in 0..3 {
            for coordinate in normal {
                normal_bytes.extend_from_slice(&coordinate);
            }
        }
    }
    let normal_buffer = buffer(cache, &normal_bytes)?;
    drop(normal_bytes);
    let mut indices = Vec::with_capacity(triangles.len() * 12);
    for index in 0..(triangles.len() as u32 * 3) {
        indices.extend_from_slice(&index.to_le_bytes());
    }
    let indices = buffer(cache, &indices)?;
    let spans: Vec<_> = counts.iter().copied().flat_map(u32::to_le_bytes).collect();
    let empty = buffer(cache, &[])?;
    let edge_count = segments.len();
    let edges = if edge_count == 0 {
        empty.clone()
    } else {
        let mut points = Vec::with_capacity(edge_count * 24);
        for edge in segments {
            for pair in edge {
                points.extend_from_slice(&((pair >> 32) as u32).to_le_bytes());
                points.extend_from_slice(&(pair as u32).to_le_bytes());
            }
        }
        buffer(cache, &points)?
    };
    let edge_spans = 1u32.to_le_bytes().repeat(edge_count);
    let bounds = stats.bounds();
    let name = entry.rsplit('/').next().unwrap_or(entry);
    let dimensions: Vec<_> = ["x", "y", "z"].into_iter().enumerate().map(|(i, axis)| {
        json!({"id":format!("model:{axis}"),"kind":"overall","axis":axis,"value":bounds["size"][i],"unit":"mm"})
    }).collect();
    let mut model = json!({"stl_center":stats.center(),"parts":[{
        "id":"model","name":name,"instance_id":"model","position":[0,0,0],
        "bounds":bounds,"volume":null,"area":stats.area,"shape_kind":"Mesh",
        "face_count":counts.len(),"edge_count":edge_count,"features":{},"accuracy":"mesh","units":"mm",
        "geometry_fingerprint":{"format":if with_edges { FORMAT } else { LEGACY_FORMAT },"sha256":digest},"dimensions":dimensions,
        "source":{"file":entry,"line":1},
        "mesh":{"format":crate::artifacts::MESH_FORMAT,"tolerance_mm":if with_edges { json!(1e-5) } else { json!(0) },
            "buffers":{"vertices":vertices,"obj_vertices":vertices,"normals":normal_buffer,
                "triangles":indices,"edges":edges,"triangles_per_face":buffer(cache,&spans)?,
                "segments_per_edge":if edge_count == 0 { empty.clone() } else { buffer(cache,&edge_spans)? },
                "edge_types":if edge_count == 0 { empty } else { buffer(cache,&vec![0;edge_spans.len()])? },
                "face_types":buffer(cache,&vec![0;spans.len()])?}}
    }],"parameters":{},"annotations":[],"units":"mm","revision":revision,"dependencies":inputs.dependencies});
    let mesh = &mut model["parts"][0]["mesh"];
    mesh["face_ids"] = Value::Array(
        (0..counts.len())
            .map(|index| Value::String(format!("face:{index}")))
            .collect(),
    );
    mesh["edge_ids"] = Value::Array(
        (0..edge_count)
            .map(|index| Value::String(format!("edge:{index}")))
            .collect(),
    );
    let records = crate::annotations::decode(&inputs.annotations)?;
    model["annotations"] = crate::annotations::reproject(&records, &model)?.ok_or_else(|| {
        Error::domain(
            "UNSUPPORTED_OPERATION",
            "STL measurements require mesh points or bounds",
        )
    })?;
    Ok(model)
}

impl Core {
    pub(crate) async fn publish_stl(
        &self,
        workspace: &str,
        revision: &str,
        inputs: Inputs,
        entry: String,
        source: Source,
        with_edges: bool,
        cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        let cache = self.root.join("artifacts").join(workspace).join(revision);
        let blobs = self.root.join("blobs");
        let revision = revision.to_owned();
        tokio::task::spawn_blocking(move || {
            check_cancel(&cancel)?;
            std::fs::create_dir_all(&cache)?;
            let _lease = crate::cache::Lease::shared(&cache)?;
            let Source {
                digest,
                bytes,
                preview,
            } = source;
            let bytes = match bytes {
                Some(bytes) => bytes,
                None => read_mesh(&blobs, &digest)?,
            };
            publish(
                &bytes,
                &cache,
                &entry,
                &digest,
                &revision,
                &inputs,
                with_edges,
                &cancel,
                preview.as_ref(),
            )
        })
        .await
        .map_err(|_| Error::domain("INVALID_GEOMETRY", "STL publication failed"))?
    }
}

fn read_vertices(bytes: &[u8]) -> impl Iterator<Item = Triangle> + '_ {
    bytes.chunks_exact(36).map(|triangle| {
        std::array::from_fn(|vertex| {
            std::array::from_fn(|axis| {
                let at = vertex * 12 + axis * 4;
                f32::from_le_bytes(triangle[at..at + 4].try_into().unwrap())
            })
        })
    })
}

enum Selection<'a> {
    Part,
    Surface(&'a [u8]),
    Edge([DVec3; 2]),
}

fn inspect(
    model: &Value,
    args: &Value,
    vertices: &[u8],
    counts: &[u8],
    edges: &[u8],
) -> Result<Value> {
    let revision = model["revision"].as_str().unwrap_or_default();
    let part = &model["parts"][0];
    let refs = args["entities"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut spans = vec![0];
    for count in counts.chunks_exact(4) {
        spans.push(spans.last().unwrap() + u32::from_le_bytes(count.try_into().unwrap()) as usize);
    }
    if spans.last().unwrap() * 36 != vertices.len() {
        return Err(Error::domain(
            "ARTIFACT_CORRUPT",
            "STL surface spans differ from vertex data",
        ));
    }
    let selected = |reference: &Value| -> Result<Selection<'_>> {
        if reference["revision"] == revision && reference["instance_id"] == "model" {
            if reference["entity_id"] == "part:0" {
                return Ok(Selection::Part);
            }
            if let Some((kind, index)) = reference["entity_id"]
                .as_str()
                .and_then(|id| id.split_once(':'))
                .and_then(|(kind, index)| Some((kind, index.parse::<usize>().ok()?)))
            {
                if kind == "face" && index < spans.len() - 1 {
                    return Ok(Selection::Surface(
                        &vertices[spans[index] * 36..spans[index + 1] * 36],
                    ));
                }
                if kind == "edge" && index < edges.len() / 24 {
                    let points = std::array::from_fn(|vertex| {
                        DVec3::from_array(std::array::from_fn(|axis| {
                            let offset = index * 24 + vertex * 12 + axis * 4;
                            f32::from_le_bytes(edges[offset..offset + 4].try_into().unwrap()) as f64
                        }))
                    });
                    return Ok(Selection::Edge(points));
                }
            }
        }
        Err(Error::domain(
            "STALE_ENTITY",
            "Unknown STL geometry selection",
        ))
    };
    let mut result = json!({"revision":revision,"pose_version":args["pose_version"],"accuracy":"mesh","units":"mm"});
    if args["query"] == "distance" {
        use parry3d_f64::{
            math::{Pose, Vector},
            query,
            shape::{Segment, SharedShape, TriMesh},
        };
        if refs.len() != 2 {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Distance needs two selections",
            ));
        }
        let lhs = selected(&refs[0])?;
        let rhs = selected(&refs[1])?;
        if matches!(lhs, Selection::Part) || matches!(rhs, Selection::Part) || refs[0] == refs[1] {
            result["distance"] = json!(0.);
            return Ok(result);
        }
        let as_shape = |selection| -> Result<SharedShape> {
            let vertices = match selection {
                Selection::Surface(vertices) => vertices,
                Selection::Edge([a, b]) => return Ok(SharedShape::new(Segment::new(a, b))),
                Selection::Part => unreachable!(),
            };
            let points = read_vertices(vertices)
                .flat_map(|triangle| triangle.map(|point| Vector::from_array(point.map(f64::from))))
                .collect();
            let indices = (0..(vertices.len() / 36) as u32)
                .map(|i| [i * 3, i * 3 + 1, i * 3 + 2])
                .collect();
            TriMesh::new(points, indices)
                .map(SharedShape::new)
                .map_err(|_| Error::domain("INVALID_GEOMETRY", "Could not measure STL triangles"))
        };
        let distance = query::distance(
            &Pose::IDENTITY,
            &*as_shape(lhs)?,
            &Pose::IDENTITY,
            &*as_shape(rhs)?,
        )
        .map_err(|_| {
            Error::domain(
                "UNSUPPORTED_OPERATION",
                "Could not measure these STL surfaces",
            )
        })?;
        result["distance"] = json!(distance.distance);
        return Ok(result);
    }
    if args["query"] == "interference" {
        return Err(Error::domain(
            "UNSUPPORTED_OPERATION",
            "Volume interference requires CAD solids",
        ));
    }
    let mut entities = Vec::with_capacity(refs.len());
    for reference in refs {
        let mut item = match selected(reference)? {
            Selection::Surface(vertices) => {
                let mut triangles = read_vertices(vertices);
                let first = triangles
                    .next()
                    .ok_or_else(|| Error::domain("INVALID_GEOMETRY", "Empty STL selection"))?;
                let mut stats = Stats::default();
                for triangle in std::iter::once(first).chain(triangles) {
                    stats.add(triangle, cross(triangle).length() / 2.);
                }
                let normal = cross(first).normalize().to_array();
                let center = stats.center();
                json!({"kind":"face","bounds":stats.bounds(),"center":center,"surface_type":"PLANE",
                "normal":normal,"world_normal":normal,"area":stats.area,
                "anchor":{"kind":"local_point","instance_id":"model","point":center}})
            }
            Selection::Edge([a, b]) => {
                let min = a.min(b);
                let max = a.max(b);
                let center = ((a + b) / 2.).to_array();
                json!({"kind":"edge", "curve_type":"LINE","length":a.distance(b),"center":center,
                "bounds":{"min":min.to_array(),"max":max.to_array(),"size":(max-min).to_array()},
                "anchor":{"kind":"local_point","instance_id":"model","point":center}})
            }
            Selection::Part => {
                let mut item = part.clone();
                item.as_object_mut().unwrap().remove("mesh");
                item["kind"] = json!("part");
                item["center"] = model["stl_center"].clone();
                item
            }
        };
        item["reference"] = reference.clone();
        item["world_bounds"] = item["bounds"].clone();
        item["world_center"] = item["center"].clone();
        item["coordinate_space"] = json!("part_local");
        item["semantic_labels"] = json!([]);
        entities.push(item);
    }
    result["entities"] = json!(entities);
    Ok(result)
}

impl Core {
    pub(crate) async fn query_stl(
        &self,
        workspace: &str,
        revision: &str,
        model: Value,
        inputs: Inputs,
        method: &str,
        args: Value,
        cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        check_cancel(&cancel)?;
        if method == "export" {
            let format = args["format"]
                .as_str()
                .unwrap_or("STEP")
                .to_ascii_uppercase();
            if format == "STEP" {
                // The derived CAD representation is scoped to this export. It
                // cannot replace the mesh revision or its selection identities.
                let (admission, _construction) = self.native_construction(cancel.clone()).await?;
                let run = self.materialize(workspace, &crate::id(), &inputs).await?;
                let source = run.source.clone();
                let cache = self.root.join("artifacts").join(workspace).join(revision);
                let mut worker = self
                    .spawn_managed_worker(cache, run, admission, cancel.clone())
                    .await?;
                let result = async {
                    worker.request(revision, "build", json!({"source_root":source,
                        "entry_point":inputs.entry_point,"expected_dependencies":inputs.dependencies,
                        "generate_mesh":false}), cancel.clone()).await?;
                    worker.request(revision, method, args, cancel).await
                }.await;
                worker.stop().await?;
                return Ok(result?["data"].clone());
            }
            if format != "STL" {
                return Err(Error::domain(
                    "UNSUPPORTED_OPERATION",
                    "Choose STEP or STL export",
                ));
            }
            let digest = model["parts"][0]["geometry_fingerprint"]["sha256"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let blobs = self.root.join("blobs");
            let cache = self.root.join("artifacts").join(workspace).join(revision);
            let handle = format!("{}.stl", uuid::Uuid::new_v4().simple());
            let revision = revision.to_owned();
            let _publication = self.display_publication.read().await;
            return tokio::task::spawn_blocking(move || {
                let bytes = read_mesh(&blobs, &digest)?;
                check_cancel(&cancel)?;
                portable::atomic_write(&cache, &handle, &bytes)?;
                Ok(
                    json!({"format":"STL","handle":handle,"sha256":digest,"bytes":bytes.len(),
                    "revision":revision,"units":"mm","pose_version":args["pose_version"]}),
                )
            })
            .await
            .map_err(|_| Error::domain("IO_ERROR", "STL export failed"))?;
        }
        let buffers = &model["parts"][0]["mesh"]["buffers"];
        let load = |name: &str| buffers[name]["handle"].as_str().unwrap_or_default();
        let has = |prefix: &str| {
            args["entities"].as_array().is_some_and(|entities| {
                entities.iter().any(|item| {
                    item["entity_id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with(prefix))
                })
            })
        };
        let faces = async {
            if has("face:") {
                tokio::try_join!(
                    self.read_artifact(workspace, revision, load("vertices")),
                    self.read_artifact(workspace, revision, load("triangles_per_face")),
                )
            } else {
                Ok((Vec::new(), Vec::new()))
            }
        };
        let edges = async {
            if has("edge:") {
                self.read_artifact(workspace, revision, load("edges")).await
            } else {
                Ok(Vec::new())
            }
        };
        let ((vertices, counts), edges) = tokio::try_join!(faces, edges)?;
        tokio::task::spawn_blocking(move || {
            check_cancel(&cancel)?;
            inspect(&model, &args, &vertices, &counts, &edges)
        })
        .await
        .map_err(|_| Error::domain("INVALID_GEOMETRY", "STL inspection failed"))?
    }
}
