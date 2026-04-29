// SPDX-License-Identifier: MIT
//! `MdsServer` glues the tool router (defined in `crate::tools`) and the
//! resource catalogue into one `ServerHandler` rmcp can drive over either
//! stdio or Streamable-HTTP.

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    model::{
        Implementation, ListResourcesResult, PaginatedRequestParams, ProtocolVersion,
        ReadResourceRequestParams, ReadResourceResult, ServerCapabilities, ServerInfo,
        SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::RequestContext,
    tool_handler, ErrorData as McpError, RoleServer, ServerHandler,
};

use crate::emulator::EmulatorActor;
use crate::resources;

pub struct MdsServer {
    actor: EmulatorActor,
    pub(crate) tool_router: ToolRouter<MdsServer>,
    subscriptions: Arc<Mutex<HashSet<String>>>,
}

impl MdsServer {
    pub fn new(actor: EmulatorActor) -> Self {
        Self {
            actor,
            tool_router: Self::tool_router(),
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn actor(&self) -> &EmulatorActor {
        &self.actor
    }
}

impl Clone for MdsServer {
    fn clone(&self) -> Self {
        Self {
            actor: self.actor.clone(),
            tool_router: Self::tool_router(),
            subscriptions: self.subscriptions.clone(),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MdsServer {
    fn get_info(&self) -> ServerInfo {
        let server_info = Implementation::new("mds-mcp", env!("CARGO_PKG_VERSION"));
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_list_changed()
                .enable_resources_subscribe()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(server_info)
        .with_instructions(
            "Megadrive Studio MCP server (M2). 19 tools across control / memory / vdp / cpu / state. \
             7 subscribable resources under mega://*. Some tools (step_instruction, screenshot, \
             breakpoints, z80 regs) return {ok:false, not_implemented:true} until M3/M4.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: resources::list_resources(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let def = resources::find(&request.uri).ok_or_else(|| {
            McpError::invalid_params(format!("unknown resource uri: {}", request.uri), None)
        })?;
        let contents = resources::read_contents(&self.actor, def).await;
        Ok(ReadResourceResult::new(vec![contents]))
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        if resources::find(&request.uri).is_none() {
            return Err(McpError::invalid_params(
                format!("unknown resource uri: {}", request.uri),
                None,
            ));
        }
        self.subscriptions.lock().insert(request.uri);
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.subscriptions.lock().remove(&request.uri);
        Ok(())
    }
}
