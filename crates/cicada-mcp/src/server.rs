//! `CicadaServer` — the rmcp adapter: it translates MCP wire types
//! into [`tools::dispatch`] calls and wraps [`ToolOutcome`] into
//! `CallToolResult`. All semantics live in [`tools`]; this module is
//! the only place rmcp types appear.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::{MaybeSendFuture, RequestContext};
use rmcp::{ErrorData as McpError, RoleServer, ServiceExt};

use crate::ctx::{ServerOptions, Session};
use crate::tools::{self, IssuedDigests, ToolDef, ToolOutcome};

/// Instructions advertised at `initialize` — the contract agents
/// must follow (single write path + the apply gate).
const INSTRUCTIONS: &str = "\
cicada manages calendars as code: desired state lives in YAML files under \
the working root; live state lives in the provider; recorded state tracks \
the last reconcile. Read tools (cicada.calendar.list, cicada.events.range, \
cicada.events.show, cicada.state.drift) are always safe. To change events, \
edit the YAML — there is no events.create (ADR-0002 §10) — then call \
cicada.plan.preview and show the user the plan. Only after the user \
approves, call cicada.plan.apply with {confirm: true, planDigest} — the \
digest proves a preview happened and is rejected when stale.";

/// The MCP server: a [`Session`] plus the issued preview digests for
/// the apply gate.
pub struct CicadaServer {
    /// Working root + provider/state wiring.
    session: Session,
    /// `planDigest`s `cicada.plan.preview` issued this session.
    issued: IssuedDigests,
}

impl CicadaServer {
    /// A server over `session`.
    pub fn new(session: Session) -> Self {
        Self {
            session,
            issued: Mutex::new(BTreeSet::new()),
        }
    }

    /// Dispatch one tool call (extracted so tests can drive it
    /// without the transport).
    async fn call(&self, name: &str, arguments: tools::Args) -> Result<CallToolResponse, McpError> {
        let outcome = tools::dispatch(&self.session, &self.issued, name, arguments).await;
        Ok(render(outcome).into())
    }
}

/// `ToolOutcome` → `CallToolResult`: text content always, structured
/// content when non-null, `isError` for failures.
fn render(outcome: ToolOutcome) -> CallToolResult {
    let mut result = if outcome.is_error {
        CallToolResult::error(vec![ContentBlock::text(outcome.text)])
    } else {
        CallToolResult::success(vec![ContentBlock::text(outcome.text)])
    };
    result.structured_content = Some(outcome.structured);
    result
}

/// `&ToolDef` → `rmcp::Tool` with hint annotations.
fn tool(def: &ToolDef) -> Tool {
    let mut annotations = ToolAnnotations::default();
    annotations.read_only_hint = Some(def.read_only);
    if !def.read_only {
        annotations.destructive_hint = Some(def.name == "cicada.plan.apply");
        annotations.idempotent_hint = Some(def.name != "cicada.plan.apply");
    }
    let schema = def.input_schema.as_object().cloned().unwrap_or_default();
    Tool::new(def.name, def.description, Arc::new(schema)).with_annotations(annotations)
}

impl ServerHandler for CicadaServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("cicada-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_
    {
        std::future::ready(Ok(ListToolsResult::with_all_items(
            tools::definitions().iter().map(tool).collect(),
        )))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools::definitions()
            .iter()
            .find(|d| d.name == name)
            .map(tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.call(&request.name, request.arguments).await
    }
}

/// Serve MCP over stdio until the client disconnects — the lifecycle
/// of `cicada-mcp` and `cicada mcp` (in-process; MCP servers are
/// spawned per session anyway, ADR-0001 §8a).
pub async fn serve_stdio(opts: &ServerOptions) -> Result<(), String> {
    let session = Session::open(opts)?;
    let service = CicadaServer::new(session)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| format!("cicada-mcp: cannot start MCP server: {e}"))?;
    service
        .waiting()
        .await
        .map_err(|e| format!("cicada-mcp: server task failed: {e}"))?;
    Ok(())
}

/// Blocking entry point — builds the runtime and serves stdio.
/// Returns the process exit code; `Session::open` errors are
/// reported on stderr (stdout is the protocol channel).
pub fn run_stdio(opts: &ServerOptions) -> std::process::ExitCode {
    // stdout is the protocol channel — send logs to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();
    // A current-thread runtime build can't fail — `runtime_failed`
    // covers the theoretical error in normal builds; under coverage
    // the unwrap is unconditional.
    #[cfg(not(coverage))]
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| runtime_failed(&e));
    #[cfg(coverage)]
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime builds");
    match runtime.block_on(serve_stdio(opts)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The tokio runtime can't even build — nothing else to do but say
/// so and exit; returning `!` keeps the call-site a single
/// `unwrap_or_else` (a runtime build failure is not realistically
/// reachable in tests, so this is compiled out under coverage).
#[cfg(not(coverage))]
fn runtime_failed(e: &dyn std::fmt::Display) -> ! {
    eprintln!("cicada-mcp: cannot start runtime: {e}");
    std::process::exit(1)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn test_session() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("calendars")).unwrap();
        std::fs::write(
            dir.path().join("cicada.yaml"),
            "apiVersion: cicada/v1\nkind: Manifest\ncalendars: [calendars/work.yaml]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("calendars/work.yaml"),
            "apiVersion: cicada/v1\nkind: Calendar\nmetadata: {name: work, timeZone: UTC}\nspec: {events: []}\n",
        )
        .unwrap();
        let session = Session::open(&ServerOptions {
            root: Some(dir.path().to_path_buf()),
            provider: None,
            fake_file: None,
            state: None,
        })
        .unwrap();
        (dir, session)
    }

    #[test]
    fn get_tool_finds_definitions_by_name() {
        let (_dir, session) = test_session();
        let server = CicadaServer::new(session);
        let found = server.get_tool("cicada.events.range").unwrap();
        assert_eq!(found.name.as_ref(), "cicada.events.range");
        assert!(server.get_tool("cicada.nope").is_none());
    }
}
