//! Official MCP SDK envelope around the live app's shared tools.
use crate::Client;
use cadpad_core::{error_value, tools};
use rmcp::{ErrorData, RoleServer, ServerHandler, model::*, service::RequestContext};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Bridge {
    connection_file: PathBuf,
    client_name: String,
    client: Arc<Mutex<Option<Arc<Client>>>>,
    definitions: Arc<Mutex<Option<Arc<Vec<tools::ToolSpec>>>>>,
}
impl Bridge {
    pub fn new(connection_file: PathBuf, client_name: String) -> Self {
        Self {
            connection_file,
            client_name,
            client: Arc::new(Mutex::new(None)),
            definitions: Arc::new(Mutex::new(None)),
        }
    }
    async fn app(&self) -> cadpad_core::Result<Arc<Client>> {
        let mut client = self.client.lock().await;
        if let Some(client) = &*client {
            return Ok(client.clone());
        }
        let connected = Arc::new(Client::connect(&self.connection_file, &self.client_name).await?);
        *client = Some(connected.clone());
        Ok(connected)
    }
    async fn live_definitions(&self) -> cadpad_core::Result<Arc<Vec<tools::ToolSpec>>> {
        let mut definitions = self.definitions.lock().await;
        if let Some(current) = &*definitions {
            return Ok(current.clone());
        }
        let current: Arc<Vec<tools::ToolSpec>> = Arc::new(serde_json::from_value(
            self.app().await?.definitions().await?,
        )?);
        *definitions = Some(current.clone());
        Ok(current)
    }
}
impl ServerHandler for Bridge {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("cadpad-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions=Some("Cadpad connects to a running desktop app. Call get_project to discover workspace IDs, then use an explicit workspace_id and expected revision. Stage Python with update_source, build the draft, poll get_job, inspect exact geometry, and repair failed drafts. Never infer exact dimensions from pixels. Python cached_shape(unique_name,builder), also context.cached_shape, optionally reuses deterministic CadQuery Shapes (.val() for Workplanes). Callbacks must depend only on project source, published parameters, managed assets and pinned dependencies; never cache I/O, time/randomness, side effects or mutable external state. Source still executes and invalid caches rebuild. No shell or independent headless workspace is exposed.".into());
        info
    }
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        // Prefer the app's actual contract. A separately installed bridge may
        // have been built from a different revision. Offline discovery remains
        // available, with APP_UNAVAILABLE returned as an ordinary tool error.
        let definitions = match self.live_definitions().await {
            Ok(definitions) => definitions,
            Err(_) => Arc::new(tools::input_definitions()),
        };
        let tools = definitions
            .iter()
            .cloned()
            .map(|tool| {
                let input = tool.input_schema.as_object().ok_or_else(|| {
                    ErrorData::internal_error("App input schema must be an object", None)
                })?;
                let mut result = Tool::new(tool.name, tool.description, input.clone());
                result.output_schema = tool
                    .mcp_output_schema
                    .or(tool.output_schema)
                    .map(|schema| {
                        schema
                            .as_object()
                            .cloned()
                            .map(std::sync::Arc::new)
                            .ok_or_else(|| {
                                ErrorData::internal_error(
                                    "App output schema must be an object",
                                    None,
                                )
                            })
                    })
                    .transpose()?;
                Ok(result)
            })
            .collect::<Result<Vec<_>, ErrorData>>()?;
        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.clone().unwrap_or_default());
        let result = match self.app().await {
            Ok(client) => match self.live_definitions().await {
                Ok(definitions) if definitions.iter().any(|tool| tool.name == request.name) => {
                    client.call(&request.name, args).await
                }
                Ok(_) => {
                    return Err(ErrorData::invalid_params(
                        "Unknown Cadpad tool",
                        Some(json!({"code":"UNSUPPORTED_OPERATION"})),
                    ));
                }
                Err(error) => Err(error),
            },
            Err(error) => {
                if tools::definition(&request.name).is_none() {
                    return Err(ErrorData::invalid_params(
                        "Unknown Cadpad tool",
                        Some(json!({"code":"UNSUPPORTED_OPERATION"})),
                    ));
                }
                Err(error)
            }
        };
        Ok(match result {
            Ok(mut value) => {
                if let Some(image) = value
                    .as_object_mut()
                    .and_then(|value| value.remove("image"))
                {
                    let mut result = CallToolResult::structured(value);
                    result.content.push(ContentBlock::image(
                        image["data"].as_str().unwrap_or("").to_string(),
                        image["mime_type"]
                            .as_str()
                            .unwrap_or("image/png")
                            .to_string(),
                    ));
                    result
                } else if request.name == "export" {
                    let workspace = args_workspace(&request.arguments);
                    let uri = format!(
                        "cadpad://{}/{}/{}",
                        workspace,
                        value["revision"].as_str().unwrap_or(""),
                        value["handle"].as_str().unwrap_or("")
                    );
                    value["resource_uri"] = json!(uri);
                    let name = format!(
                        "assembly.{}",
                        value["format"].as_str().unwrap_or("STEP").to_lowercase()
                    );
                    let mut result = CallToolResult::structured(value);
                    result
                        .content
                        .push(ContentBlock::resource_link(Resource::new(uri, name)));
                    result
                } else {
                    CallToolResult::structured(value)
                }
            }
            Err(error) => CallToolResult::structured_error(error_value(&error)),
        }
        .into())
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let (path, range) = parse_resource_uri(&request.uri)?;
        let client = self.app().await.map_err(resource_error)?;
        let value = if let Some(range) = range {
            client
                .artifact_chunk(&path[0], &path[1], &path[2], range)
                .await
        } else {
            client.artifact(&path[0], &path[1], &path[2]).await
        }
        .map_err(resource_error)?;
        if let Some(manifest) = value.get("manifest") {
            return Ok(ReadResourceResult::new(vec![
                ResourceContents::text(manifest.to_string(), request.uri)
                    .with_mime_type("application/vnd.cadpad.export-manifest+json"),
            ])
            .into());
        }
        Ok(ReadResourceResult::new(vec![
            ResourceContents::blob(
                value["blob"].as_str().unwrap_or("").to_string(),
                request.uri,
            )
            .with_mime_type(
                value["mime_type"]
                    .as_str()
                    .unwrap_or("application/octet-stream"),
            ),
        ])
        .into())
    }
}

fn args_workspace(arguments: &Option<serde_json::Map<String, Value>>) -> &str {
    arguments
        .as_ref()
        .and_then(|arguments| arguments.get("workspace_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn resource_error(error: cadpad_core::Error) -> ErrorData {
    ErrorData::resource_not_found(error.to_string(), Some(error_value(&error)))
}
fn parse_resource_uri(
    uri: &str,
) -> Result<([String; 3], Option<cadpad_core::artifacts::Chunk>), ErrorData> {
    let invalid = || ErrorData::invalid_params("Invalid Cadpad export URI or byte range", None);
    if uri.len() > 1024 || uri.contains('#') || uri.contains('%') {
        return Err(invalid());
    }
    let (path, query) = uri
        .strip_prefix("cadpad://")
        .ok_or_else(invalid)?
        .split_once('?')
        .map_or(
            (uri.strip_prefix("cadpad://").unwrap(), None),
            |(path, query)| (path, Some(query)),
        );
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return Err(invalid());
    }
    let range = if let Some(query) = query {
        let mut values = std::collections::BTreeMap::new();
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').ok_or_else(invalid)?;
            if values.insert(key, value).is_some() {
                return Err(invalid());
            }
        }
        if values.len() != 3 {
            return Err(invalid());
        }
        let number = |key| -> Result<u64, ErrorData> {
            let value = values.get(key).ok_or_else(invalid)?;
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            value.parse().map_err(|_| invalid())
        };
        Some(cadpad_core::artifacts::Chunk {
            offset: number("offset")?,
            length: number("length")?,
            content_id: values.get("content_id").ok_or_else(invalid)?.to_string(),
        })
    } else {
        None
    };
    Ok(([parts[0].into(), parts[1].into(), parts[2].into()], range))
}

#[cfg(test)]
mod uri_tests {
    use super::*;
    #[test]
    fn range_uris_are_strict_and_preserve_explicit_scope() {
        let uri = "cadpad://workspace/revision/0123456789abcdef0123456789abcdef.step";
        let (scope, range) = parse_resource_uri(uri).unwrap();
        assert_eq!(scope[0], "workspace");
        assert!(range.is_none());
        let (_, range) = parse_resource_uri(&format!(
            "{uri}?offset=4096&length=100&content_id={}",
            "a".repeat(64)
        ))
        .unwrap();
        assert_eq!(range.unwrap().offset, 4096);
        for query in [
            "offset=-1&length=2&content_id=a",
            "offset=0&length=2&content_id=a&offset=1",
            "offset=0&length=2",
            "offset=0&length=2&other=a",
            "offset=18446744073709551616&length=2&content_id=a",
        ] {
            assert!(parse_resource_uri(&format!("{uri}?{query}")).is_err());
        }
        for uri in [
            "file:///etc/passwd",
            "cadpad://w/r/h/extra",
            "cadpad://w/r/h#fragment",
            "cadpad://w/r/%2fetc",
        ] {
            assert!(parse_resource_uri(uri).is_err());
        }
    }
}
