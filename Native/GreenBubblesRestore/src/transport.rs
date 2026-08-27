use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::archive::{ensure_private_directory, ensure_private_regular_file};
use crate::connector::{
    ConnectorDestination, ConnectorOperation, ConnectorRequest, ConnectorResponse,
    ConnectorService, CONNECTOR_API_VERSION,
};
use crate::RestoreError;

const MAX_CONNECTOR_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_CONNECTOR_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

pub fn serve_unix(service: &ConnectorService<'_>, socket_path: &Path) -> Result<(), RestoreError> {
    let listener = bind_private_socket(socket_path)?;
    let _lease = SocketLease(socket_path.to_path_buf());
    for connection in listener.incoming() {
        let mut connection = connection?;
        handle_connection(service, &mut connection)?;
    }
    Ok(())
}

pub fn serve_unix_once(
    service: &ConnectorService<'_>,
    socket_path: &Path,
) -> Result<(), RestoreError> {
    let listener = bind_private_socket(socket_path)?;
    let _lease = SocketLease(socket_path.to_path_buf());
    let (mut connection, _) = listener.accept()?;
    handle_connection(service, &mut connection)
}

pub fn send_unix_request(
    socket_path: &Path,
    request: &ConnectorRequest,
) -> Result<ConnectorResponse, RestoreError> {
    let mut validation = validate_socket(socket_path);
    for _ in 0..1_000 {
        if validation.is_ok() {
            break;
        }
        std::thread::yield_now();
        validation = validate_socket(socket_path);
    }
    validation?;
    let mut stream = UnixStream::connect(socket_path)?;
    let bytes = serde_json::to_vec(request)?;
    if bytes.len() as u64 > MAX_CONNECTOR_REQUEST_BYTES {
        return Err(RestoreError::Integrity(
            "connector request exceeds its byte limit".to_string(),
        ));
    }
    stream.write_all(&bytes)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = Vec::new();
    stream
        .take(MAX_CONNECTOR_RESPONSE_BYTES + 1)
        .read_to_end(&mut response)?;
    if response.len() as u64 > MAX_CONNECTOR_RESPONSE_BYTES {
        return Err(RestoreError::Integrity(
            "connector response exceeds its byte limit".to_string(),
        ));
    }
    Ok(serde_json::from_slice(&response)?)
}

pub fn load_connector_request(path: &Path) -> Result<ConnectorRequest, RestoreError> {
    ensure_private_regular_file(path)?;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_CONNECTOR_REQUEST_BYTES {
        return Err(RestoreError::Integrity(
            "connector request exceeds its byte limit".to_string(),
        ));
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub fn run_mcp_adapter(
    socket_path: &Path,
    requester_id: &str,
    destination: ConnectorDestination,
) -> Result<(), RestoreError> {
    if requester_id.is_empty() || requester_id.len() > 256 {
        return Err(RestoreError::Integrity(
            "MCP requester ID must be between 1 and 256 bytes".to_string(),
        ));
    }
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                write_mcp(
                    &mut stdout,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":error.to_string()}}),
                )?;
                continue;
            }
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let id = request.get("id").cloned();
        if id.is_none() {
            continue;
        }
        let id = id.unwrap_or(Value::Null);
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "greenbubbles", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
            "ping" => json!({"jsonrpc":"2.0","id":id,"result":{}}),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": mcp_tools()}
            }),
            "tools/call" => match mcp_call(socket_path, requester_id, destination, &request, &id) {
                Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
                Err(error) => json!({
                    "jsonrpc":"2.0",
                    "id":id,
                    "result":{
                        "content":[{"type":"text","text":error.to_string()}],
                        "isError":true
                    }
                }),
            },
            _ => json!({
                "jsonrpc":"2.0",
                "id":id,
                "error":{"code":-32601,"message":"Method not found"}
            }),
        };
        write_mcp(&mut stdout, &response)?;
    }
    Ok(())
}

fn bind_private_socket(path: &Path) -> Result<UnixListener, RestoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| RestoreError::UnsafePath("socket path has no parent".to_string()))?;
    ensure_private_directory(parent)?;
    if path.try_exists()? {
        return Err(RestoreError::Integrity(
            "connector socket path already exists".to_string(),
        ));
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    validate_socket(path)?;
    Ok(listener)
}

fn validate_socket(path: &Path) -> Result<(), RestoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(RestoreError::Integrity(
            "connector socket must be an owner-only Unix socket".to_string(),
        ));
    }
    Ok(())
}

fn handle_connection(
    service: &ConnectorService<'_>,
    connection: &mut UnixStream,
) -> Result<(), RestoreError> {
    let mut bytes = Vec::new();
    connection
        .take(MAX_CONNECTOR_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let response = if bytes.len() as u64 > MAX_CONNECTOR_REQUEST_BYTES {
        invalid_transport_response("transport", "connector request exceeds its byte limit")
    } else {
        match serde_json::from_slice::<ConnectorRequest>(&bytes) {
            Ok(request) => service.handle(request),
            Err(error) => invalid_transport_response("transport", &error.to_string()),
        }
    };
    let encoded = serde_json::to_vec(&response)?;
    if encoded.len() as u64 > MAX_CONNECTOR_RESPONSE_BYTES {
        return Err(RestoreError::Integrity(
            "connector response exceeds its byte limit".to_string(),
        ));
    }
    connection.write_all(&encoded)?;
    connection.flush()?;
    Ok(())
}

fn invalid_transport_response(request_id: &str, message: &str) -> ConnectorResponse {
    ConnectorResponse {
        api_version: CONNECTOR_API_VERSION.to_string(),
        request_id: request_id.to_string(),
        ok: false,
        result: None,
        error: Some(crate::connector::ConnectorErrorBody {
            code: crate::connector::ConnectorErrorCode::InvalidRequest,
            message: message.to_string(),
            retryable: false,
        }),
    }
}

fn mcp_call(
    socket_path: &Path,
    requester_id: &str,
    destination: ConnectorDestination,
    request: &Value,
    id: &Value,
) -> Result<Value, RestoreError> {
    let params = request
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| RestoreError::Integrity("MCP tools/call has no params".to_string()))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RestoreError::Integrity("MCP tool call has no name".to_string()))?;
    let kind = mcp_operation_kind(name).ok_or_else(|| {
        RestoreError::Integrity(format!("unsupported GreenBubbles MCP tool: {name}"))
    })?;
    let mut arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    arguments.insert("kind".to_string(), Value::String(kind.to_string()));
    let operation: ConnectorOperation = serde_json::from_value(Value::Object(arguments))?;
    let connector_request = ConnectorRequest {
        api_version: CONNECTOR_API_VERSION.to_string(),
        request_id: mcp_request_id(id),
        requester_id: requester_id.to_string(),
        destination,
        operation,
    };
    let response = send_unix_request(socket_path, &connector_request)?;
    let text = serde_json::to_string(&response)?;
    Ok(json!({
        "content": [{"type":"text","text":text}],
        "structuredContent": response,
        "isError": !response.ok
    }))
}

fn mcp_request_id(id: &Value) -> String {
    match id {
        Value::String(value) => value.clone(),
        _ => id.to_string(),
    }
}

fn mcp_operation_kind(name: &str) -> Option<&'static str> {
    Some(match name {
        "greenbubbles_capabilities" => "capabilities",
        "greenbubbles_status" => "status",
        "greenbubbles_coverage" => "coverage",
        "greenbubbles_get_changes" => "getChanges",
        "greenbubbles_list_conversations" => "listConversations",
        "greenbubbles_search_messages" => "searchMessages",
        "greenbubbles_get_messages" => "getMessages",
        "greenbubbles_get_message" => "getMessage",
        "greenbubbles_resolve_contact" => "resolveContact",
        "greenbubbles_resolve_conversation" => "resolveConversation",
        "greenbubbles_create_message_draft" => "createMessageDraft",
        "greenbubbles_create_reply_draft" => "createReplyDraft",
        "greenbubbles_create_attachment_draft" => "createAttachmentDraft",
        "greenbubbles_preview_action" => "previewAction",
        _ => return None,
    })
}

fn mcp_tools() -> Vec<Value> {
    vec![
        tool(
            "greenbubbles_capabilities",
            "Report each read, draft, and send capability independently",
            object_schema(&[]),
        ),
        tool(
            "greenbubbles_status",
            "Read encrypted-replica freshness, compatibility, counts, and health",
            object_schema(&[]),
        ),
        tool(
            "greenbubbles_coverage",
            "Read exact schema, semantic, relationship, and artifact coverage",
            object_schema(&[]),
        ),
        tool(
            "greenbubbles_get_changes",
            "Read the resumable conversation-scoped change stream",
            object_schema(&[("cursor", "string", false), ("limit", "integer", false)]),
        ),
        tool(
            "greenbubbles_list_conversations",
            "List only conversations enabled by deterministic local policy",
            object_schema(&[]),
        ),
        tool(
            "greenbubbles_search_messages",
            "Search exact replica text inside policy scopes",
            object_schema(&[
                ("query", "string", true),
                ("conversationId", "string", false),
                ("cursor", "string", false),
                ("limit", "integer", false),
            ]),
        ),
        tool(
            "greenbubbles_get_messages",
            "Page canonical messages in one authorized conversation",
            object_schema(&[
                ("conversationId", "string", true),
                ("cursor", "string", false),
                ("limit", "integer", false),
            ]),
        ),
        tool(
            "greenbubbles_get_message",
            "Get one canonical message if its conversation and time are authorized",
            object_schema(&[("canonicalId", "string", true)]),
        ),
        tool(
            "greenbubbles_resolve_contact",
            "Resolve a stable participant to local human-readable evidence",
            object_schema(&[("participantId", "string", true)]),
        ),
        tool(
            "greenbubbles_resolve_conversation",
            "Resolve an authorized conversation to exact recipient evidence",
            object_schema(&[("conversationId", "string", true)]),
        ),
        tool(
            "greenbubbles_create_message_draft",
            "Create an immutable non-executing text/attachment draft",
            object_schema(&[
                ("conversationId", "string", true),
                ("renderedText", "string", true),
                ("attachmentIds", "array", false),
                ("expiresInSeconds", "integer", false),
            ]),
        ),
        tool(
            "greenbubbles_create_reply_draft",
            "Create an immutable non-executing reply bound to one message",
            object_schema(&[
                ("conversationId", "string", true),
                ("replyTargetCanonicalId", "string", true),
                ("renderedText", "string", true),
                ("attachmentIds", "array", false),
                ("expiresInSeconds", "integer", false),
            ]),
        ),
        tool(
            "greenbubbles_create_attachment_draft",
            "Create an immutable non-executing attachment draft with verified digests",
            object_schema(&[
                ("conversationId", "string", true),
                ("attachmentIds", "array", true),
                ("renderedText", "string", false),
                ("expiresInSeconds", "integer", false),
            ]),
        ),
        tool(
            "greenbubbles_preview_action",
            "Preview exact immutable draft and recipient evidence; never executes it",
            object_schema(&[("draftId", "string", true)]),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema})
}

fn object_schema(properties: &[(&str, &str, bool)]) -> Value {
    let mut fields = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, kind, is_required) in properties {
        let schema = if *kind == "array" {
            json!({"type":"array","items":{"type":"string"},"maxItems":20})
        } else {
            json!({"type":kind})
        };
        fields.insert((*name).to_string(), schema);
        if *is_required {
            required.push(*name);
        }
    }
    json!({
        "type":"object",
        "properties":fields,
        "required":required,
        "additionalProperties":false
    })
}

fn write_mcp(output: &mut impl Write, value: &Value) -> Result<(), RestoreError> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

struct SocketLease(PathBuf);

impl Drop for SocketLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

trait FileTypeSocket {
    fn is_socket(&self) -> bool;
}

impl FileTypeSocket for fs::FileType {
    fn is_socket(&self) -> bool {
        std::os::unix::fs::FileTypeExt::is_socket(self)
    }
}
