//! WebSocket agent chat handler (v2) — backed by the full `run_tool_call_loop` engine.
//!
//! Connect: `ws://host:port/ws/chat/v2?session_id=ID&name=My+Session`
//!
//! Protocol (superset of v1):
//! ```text
//! Client -> Server: {"type":"message","content":"Hello"}
//! Client -> Server: {"type":"cancel"}
//! Server -> Client: {"type":"session_start","session_id":"...","name":"..."}
//! Server -> Client: {"type":"chunk","content":"..."}
//! Server -> Client: {"type":"progress","content":"..."}
//! Server -> Client: {"type":"chunk_reset"}
//! Server -> Client: {"type":"done","full_response":"..."}
//! Server -> Client: {"type":"cancelled"}
//! Server -> Client: {"type":"error","message":"...","code":"..."}
//! ```

use super::AppState;
use crate::agent::loop_::{DraftEvent, ToolLoopCancelled};
use crate::agent::memory_loader::MemoryLoader;
use crate::approval::ApprovalManager;
use crate::config::Config;
use crate::memory::Memory;
use crate::observability::Observer;
use crate::providers::traits::{ChatMessage, Provider};
use crate::security::SecurityPolicy;
use crate::tools::Tool;

use anyhow::Result;
use axum::{
    extract::{
        Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, header},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const WS_V2_PROTOCOL: &str = "zeroclaw.v2";
const BEARER_SUBPROTO_PREFIX: &str = "bearer.";
const GW_V2_SESSION_PREFIX: &str = "gw2_";

#[derive(serde::Deserialize)]
pub struct WsQueryV2 {
    pub token: Option<String>,
    pub session_id: Option<String>,
    pub name: Option<String>,
}

/// All state needed for a single WS v2 connection.
struct WsSession {
    provider: Box<dyn Provider>,
    provider_name: String,
    model: String,
    temperature: f64,
    tools: Vec<Box<dyn Tool>>,
    activated_tools: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    approval: ApprovalManager,
    hooks: Option<Arc<crate::hooks::HookRunner>>,
    observer: Arc<dyn Observer>,
    memory: Arc<dyn Memory>,
    #[allow(dead_code)]
    security: Arc<SecurityPolicy>,
    history: Vec<ChatMessage>,
    system_prompt: String,
    memory_loader: crate::agent::memory_loader::DefaultMemoryLoader,
    pacing: crate::config::PacingConfig,
    multimodal: crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    max_tool_result_chars: usize,
    context_token_budget: usize,
    excluded_tools: Vec<String>,
    dedup_exempt_tools: Vec<String>,
    auto_save: bool,
    cancel_token: CancellationToken,
}

impl WsSession {
    async fn from_config(config: &Config) -> Result<Self> {
        let observer: Arc<dyn Observer> =
            Arc::from(crate::observability::create_observer(&config.observability));
        let runtime: Arc<dyn crate::runtime::RuntimeAdapter> =
            Arc::from(crate::runtime::create_runtime(&config.runtime)?);
        let security = Arc::new(SecurityPolicy::from_config(
            &config.autonomy,
            &config.workspace_dir,
        ));
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory_with_storage_and_routes(
                &config.memory,
                &config.embedding_routes,
                Some(&config.storage.provider.config),
                &config.workspace_dir,
                config.api_key.as_deref(),
            )?);

        let composio_key = config
            .composio
            .enabled
            .then(|| config.composio.api_key.as_deref())
            .flatten();
        let composio_entity_id = config
            .composio
            .enabled
            .then_some(config.composio.entity_id.as_str());

        let (mut tools, _delegate, _reaction, _chan_map, _ask_user, _escalate) =
            crate::tools::all_tools_with_runtime(
                Arc::new(config.clone()),
                &security,
                runtime,
                memory.clone(),
                composio_key,
                composio_entity_id,
                &config.browser,
                &config.http_request,
                &config.web_fetch,
                &config.workspace_dir,
                &config.agents,
                config.api_key.as_deref(),
                config,
                None,
            );

        // MCP tools (non-fatal)
        let mut activated_tools: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>> =
            None;
        if config.mcp.enabled && !config.mcp.servers.is_empty() {
            match crate::tools::McpRegistry::connect_all(&config.mcp.servers).await {
                Ok(registry) => {
                    let registry = Arc::new(registry);
                    if config.mcp.deferred_loading {
                        let deferred =
                            crate::tools::DeferredMcpToolSet::from_registry(Arc::clone(&registry))
                                .await;
                        let activated =
                            Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
                        activated_tools = Some(Arc::clone(&activated));
                        tools.push(Box::new(crate::tools::ToolSearchTool::new(
                            deferred, activated,
                        )));
                    } else {
                        for name in registry.tool_names() {
                            if let Some(def) = registry.get_tool_def(&name).await {
                                let wrapper: Arc<dyn Tool> =
                                    Arc::new(crate::tools::McpToolWrapper::new(
                                        name,
                                        def,
                                        Arc::clone(&registry),
                                    ));
                                tools.push(Box::new(crate::tools::ArcToolRef(wrapper)));
                            }
                        }
                    }
                }
                Err(e) => tracing::error!("MCP registry failed: {e:#}"),
            }
        }

        let provider_name = config
            .default_provider
            .as_deref()
            .unwrap_or("openrouter")
            .to_string();
        let model = config
            .default_model
            .as_deref()
            .unwrap_or("anthropic/claude-sonnet-4-20250514")
            .to_string();

        let opts = crate::providers::provider_runtime_options_from_config(config);
        let provider = crate::providers::create_routed_provider_with_options(
            &provider_name,
            config.api_key.as_deref(),
            config.api_url.as_deref(),
            &config.reliability,
            &config.model_routes,
            &model,
            &opts,
        )?;

        let approval = ApprovalManager::for_non_interactive(&config.autonomy);

        let hooks = if config.hooks.enabled {
            let mut runner = crate::hooks::HookRunner::new();
            if config.hooks.builtin.command_logger {
                runner.register(Box::new(crate::hooks::builtin::CommandLoggerHook::new()));
            }
            if config.hooks.builtin.webhook_audit.enabled {
                runner.register(Box::new(crate::hooks::builtin::WebhookAuditHook::new(
                    config.hooks.builtin.webhook_audit.clone(),
                )));
            }
            Some(Arc::new(runner))
        } else {
            None
        };

        // Build system prompt while we still have the full Config.
        let tool_descs: Vec<(&str, &str)> =
            tools.iter().map(|t| (t.name(), t.description())).collect();
        let skills = crate::skills::load_skills_with_config(&config.workspace_dir, config);
        let native_tools = provider.supports_native_tools() && !tools.is_empty();
        let system_prompt = crate::channels::build_system_prompt_with_mode_and_autonomy(
            &config.workspace_dir,
            &model,
            &tool_descs,
            &skills,
            Some(&config.identity),
            None,
            Some(&config.autonomy),
            native_tools,
            config.skills.prompt_injection_mode,
            false,
            0,
        );

        Ok(Self {
            provider,
            provider_name,
            model,
            temperature: config.default_temperature,
            tools,
            activated_tools,
            approval,
            hooks,
            observer,
            memory,
            security,
            history: Vec::new(),
            system_prompt,
            memory_loader: crate::agent::memory_loader::DefaultMemoryLoader::new(
                5,
                config.memory.min_relevance_score,
            ),
            pacing: config.pacing.clone(),
            multimodal: config.multimodal.clone(),
            max_tool_iterations: config.agent.max_tool_iterations,
            max_tool_result_chars: config.agent.max_tool_result_chars,
            context_token_budget: config.agent.max_context_tokens,
            excluded_tools: config.autonomy.non_cli_excluded_tools.clone(),
            dedup_exempt_tools: config.agent.tool_call_dedup_exempt.clone(),
            auto_save: config.memory.auto_save,
            cancel_token: CancellationToken::new(),
        })
    }
}

// ── Auth helper (mirrors ws.rs) ──────────────────────────────────────────

fn extract_ws_token<'a>(headers: &'a HeaderMap, query_token: Option<&'a str>) -> Option<&'a str> {
    if let Some(t) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .filter(|t| !t.is_empty())
    {
        return Some(t);
    }
    if let Some(t) = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|protos| {
            protos
                .split(',')
                .map(|p| p.trim())
                .find_map(|p| p.strip_prefix(BEARER_SUBPROTO_PREFIX))
        })
        .filter(|t| !t.is_empty())
    {
        return Some(t);
    }
    query_token.filter(|t| !t.is_empty())
}

// ── Handler ──────────────────────────────────────────────────────────────

/// GET /ws/chat/v2 — WebSocket upgrade for full-engine agent chat.
pub async fn handle_ws_chat_v2(
    State(state): State<AppState>,
    Query(params): Query<WsQueryV2>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if state.pairing.require_pairing() {
        let token = extract_ws_token(&headers, params.token.as_deref()).unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    }

    let ws = if headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map_or(false, |protos| {
            protos.split(',').any(|p| p.trim() == WS_V2_PROTOCOL)
        }) {
        ws.protocols([WS_V2_PROTOCOL])
    } else {
        ws
    };

    let session_id = params.session_id;
    let session_name = params.name;
    ws.on_upgrade(move |socket| handle_socket_v2(socket, state, session_id, session_name))
        .into_response()
}

// ── Socket lifecycle ─────────────────────────────────────────────────────

async fn handle_socket_v2(
    socket: WebSocket,
    state: AppState,
    session_id: Option<String>,
    session_name: Option<String>,
) {
    let (mut sender, mut receiver) = socket.split();

    let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_key = format!("{GW_V2_SESSION_PREFIX}{session_id}");
    let session_name = session_name.unwrap_or_else(|| "WS v2 Session".to_string());

    let config = state.config.lock().clone();
    let mut session = match WsSession::from_config(&config).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "WS v2: failed to build session");
            let _ = send_json(
                &mut sender,
                serde_json::json!({
                    "type": "error",
                    "message": format!("Agent init failed: {e}"),
                    "code": "AGENT_INIT_FAILED",
                }),
            )
            .await;
            return;
        }
    };

    // Restore persisted history and session name.
    let mut effective_name = session_name.clone();
    let resumed_count = state
        .session_backend
        .as_ref()
        .map(|b| {
            // Ensure metadata row exists so set_session_state / set_session_name work on first turn.
            let _ = b.ensure_session(&session_key);

            let msgs = b.load(&session_key);
            let n = msgs.len();
            if n > 0 {
                session.history = msgs;
            }
            if !session_name.is_empty() {
                let _ = b.set_session_name(&session_key, &session_name);
            } else if let Ok(Some(stored)) = b.get_session_name(&session_key) {
                effective_name = stored;
            }
            n
        })
        .unwrap_or(0);

    let _ = send_json(
        &mut sender,
        serde_json::json!({
            "type": "session_start",
            "session_id": session_id,
            "name": effective_name,
            "resumed": resumed_count > 0,
            "message_count": resumed_count,
            "engine": "v2",
        }),
    )
    .await;

    // Cloud report: new session created (skip for resumed sessions).
    if resumed_count == 0 {
        spawn_cloud_report(&config, &session_id, &effective_name, "");
    }

    let mut broadcast_rx = state.event_tx.subscribe();
    let mut first_message_reported = resumed_count > 0;

    loop {
        tokio::select! {
            client_msg = receiver.next() => {
                let Some(msg) = client_msg else { break };
                let Ok(msg) = msg else { break };
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    _ => continue,
                };

                let parsed: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = send_json(&mut sender, serde_json::json!({
                            "type": "error", "message": "Invalid JSON", "code": "INVALID_JSON",
                        })).await;
                        continue;
                    }
                };

                match parsed["type"].as_str().unwrap_or("") {
                    "message" => {
                        let content = parsed["content"].as_str().unwrap_or("").trim();
                        if content.is_empty() {
                            let _ = send_json(&mut sender, serde_json::json!({
                                "type": "error", "message": "Empty content", "code": "EMPTY_CONTENT",
                            })).await;
                            continue;
                        }

                        if !first_message_reported {
                            first_message_reported = true;
                            let name = truncate_chars(content, 32);
                            let desc = truncate_chars(content, 128);

                            if let Some(ref backend) = state.session_backend {
                                let _ = backend.set_session_name(&session_key, &name);
                            }

                            spawn_cloud_report(&config, &session_id, &name, &desc);
                        }

                        session.cancel_token = CancellationToken::new();
                        process_turn(
                            &state, &mut session, &mut sender, &mut receiver,
                            content, &session_key,
                        ).await;
                    }
                    other => {
                        let _ = send_json(&mut sender, serde_json::json!({
                            "type": "error",
                            "message": format!("Unknown type \"{other}\""),
                            "code": "UNKNOWN_TYPE",
                        })).await;
                    }
                }
            }

            event = broadcast_rx.recv() => {
                if let Ok(ev) = event {
                    let _ = sender.send(Message::Text(ev.to_string().into())).await;
                }
            }
        }
    }
}

// ── Single turn ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
async fn process_turn(
    state: &AppState,
    session: &mut WsSession,
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    receiver: &mut futures_util::stream::SplitStream<WebSocket>,
    content: &str,
    session_key: &str,
) {
    let _ = state.event_tx.send(serde_json::json!({
        "type": "agent_start",
        "provider": session.provider_name,
        "model": session.model,
        "engine": "v2",
    }));

    if let Some(ref backend) = state.session_backend {
        let turn_id = uuid::Uuid::new_v4().to_string();
        let _ = backend.set_session_state(session_key, "running", Some(&turn_id));
    }

    // Inject system prompt if this is the first turn.
    if session.history.is_empty() {
        session
            .history
            .push(ChatMessage::system(session.system_prompt.clone()));
    }

    // Memory context retrieval.
    let mem_ctx = session
        .memory_loader
        .load_context(session.memory.as_ref(), content, None)
        .await
        .unwrap_or_default();

    if session.auto_save {
        let _ = session
            .memory
            .store(
                "user_msg",
                content,
                crate::memory::MemoryCategory::Conversation,
                None,
            )
            .await;
    }

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    let enriched = if mem_ctx.is_empty() {
        format!("[{now}] {content}")
    } else {
        format!("{mem_ctx}[{now}] {content}")
    };
    session.history.push(ChatMessage::user(enriched));

    // Persist user message immediately.
    if let Some(ref backend) = state.session_backend {
        let _ = backend.append(session_key, session.history.last().unwrap());
    }

    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);
    let cancel = session.cancel_token.clone();

    let loop_fut = crate::agent::loop_::run_tool_call_loop(
        session.provider.as_ref(),
        &mut session.history,
        &session.tools,
        session.observer.as_ref(),
        &session.provider_name,
        &session.model,
        session.temperature,
        true,
        Some(&session.approval),
        "ws_v2",
        None,
        &session.multimodal,
        session.max_tool_iterations,
        Some(cancel.clone()),
        Some(delta_tx),
        session.hooks.as_deref(),
        &session.excluded_tools,
        &session.dedup_exempt_tools,
        session.activated_tools.as_ref(),
        None,
        &session.pacing,
        session.max_tool_result_chars,
        session.context_token_budget,
        None,
    );
    tokio::pin!(loop_fut);

    let mut full_response = String::new();
    let mut cancelled = false;

    loop {
        tokio::select! {
            biased;

            event = delta_rx.recv() => {
                match event {
                    Some(DraftEvent::Content(delta)) => {
                        full_response.push_str(&delta);
                        let _ = send_json(sender, serde_json::json!({
                            "type": "chunk", "content": delta,
                        })).await;
                    }
                    Some(DraftEvent::Thinking(delta)) => {
                        let _ = send_json(sender, serde_json::json!({
                            "type": "thinking", "content": delta,
                        })).await;
                    }
                    Some(DraftEvent::ToolCallStart { name, args }) => {
                        let _ = send_json(sender, serde_json::json!({
                            "type": "tool_call", "name": name, "args": args,
                        })).await;
                    }
                    Some(DraftEvent::ToolCallResult { name, output }) => {
                        let _ = send_json(sender, serde_json::json!({
                            "type": "tool_result", "name": name, "output": output,
                        })).await;
                    }
                    Some(DraftEvent::ToolChunk { name, content }) => {
                        let _ = send_json(sender, serde_json::json!({
                            "type": "tool_chunk", "name": name, "content": content,
                        })).await;
                    }
                    Some(DraftEvent::Progress(text)) => {
                        let _ = send_json(sender, serde_json::json!({
                            "type": "progress", "content": text,
                        })).await;
                    }
                    Some(DraftEvent::Clear) => {
                        full_response.clear();
                        let _ = send_json(sender, serde_json::json!({ "type": "chunk_reset" })).await;
                    }
                    None => {
                        // Channel closed — loop will finish momentarily.
                    }
                }
            }

            frame = receiver.next() => {
                if let Some(Ok(Message::Text(text))) = frame {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if v["type"].as_str() == Some("cancel") {
                            cancel.cancel();
                            cancelled = true;
                        }
                    }
                }
            }

            result = &mut loop_fut => {
                match result {
                    Ok(text) => {
                        if full_response != text {
                            full_response = text;
                        }
                        let _ = send_json(sender, serde_json::json!({ "type": "chunk_reset" })).await;
                        let _ = send_json(sender, serde_json::json!({
                            "type": "done", "full_response": full_response,
                        })).await;
                    }
                    Err(e) => {
                        if e.downcast_ref::<ToolLoopCancelled>().is_some() || cancelled {
                            let _ = send_json(sender, serde_json::json!({ "type": "cancelled" })).await;
                        } else {
                            let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                            let error_code = if sanitized.to_lowercase().contains("api key")
                                || sanitized.to_lowercase().contains("authentication")
                                || sanitized.to_lowercase().contains("unauthorized")
                            {
                                "AUTH_ERROR"
                            } else if sanitized.to_lowercase().contains("provider")
                                || sanitized.to_lowercase().contains("model")
                            {
                                "PROVIDER_ERROR"
                            } else {
                                "AGENT_ERROR"
                            };
                            let _ = send_json(sender, serde_json::json!({
                                "type": "error", "message": sanitized, "code": error_code,
                            })).await;
                        }
                    }
                }
                break;
            }
        }
    }

    // Persist assistant reply.
    if let Some(ref backend) = state.session_backend {
        if !full_response.is_empty() {
            let _ = backend.append(session_key, &ChatMessage::assistant(full_response.clone()));
        }
        let _ = backend.set_session_state(session_key, "idle", None);
    }

    if session.auto_save && !full_response.is_empty() {
        let _ = session
            .memory
            .store(
                "assistant_msg",
                &full_response,
                crate::memory::MemoryCategory::Conversation,
                None,
            )
            .await;
    }

    // Fire-and-forget memory consolidation (extracts facts → Daily + Core).
    if state.auto_save && !full_response.is_empty() {
        let provider = Arc::clone(&state.provider);
        let model = state.model.clone();
        let mem = Arc::clone(&state.mem);
        let user_msg = content.to_string();
        let assistant_resp = full_response.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::memory::consolidation::consolidate_turn(
                provider.as_ref(),
                &model,
                mem.as_ref(),
                &user_msg,
                &assistant_resp,
            )
            .await
            {
                tracing::debug!("WS v2 memory consolidation skipped: {e}");
            }
        });
    }

    let _ = state.event_tx.send(serde_json::json!({
        "type": "agent_end", "engine": "v2",
    }));
}

// ── Helpers ──────────────────────────────────────────────────────────────

async fn send_json(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    value: serde_json::Value,
) -> Result<(), axum::Error> {
    sender.send(Message::Text(value.to_string().into())).await
}

/// Spawn a fire-and-forget cloud session report if credentials are available.
fn spawn_cloud_report(config: &Config, session_id: &str, name: &str, description: &str) {
    let url = match config.seewo_cloud.session_record_url {
        Some(ref u) if !u.is_empty() => u.clone(),
        _ => return,
    };
    let app_code = match config.seewo_cloud.app_code {
        Some(ref c) if !c.is_empty() => c.clone(),
        _ => return,
    };
    let token = match super::sw_state::get_sw_token() {
        Some(t) => t,
        None => return,
    };
    let uid = session_id.to_owned();
    let name = name.to_owned();
    let desc = description.to_owned();
    tokio::spawn(super::cloud_report::report_session(
        url, token, app_code, uid, name, desc,
    ));
}

/// Truncate a string to at most `max` characters (Unicode-aware).
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
