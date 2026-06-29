//! # CKOS Plugin SDK
//!
//! Defines the plugin taxonomy (§901), the unified [`Tool`] abstraction (§918),
//! the [`ToolRegistry`] (§917) and the permission tokens tools must hold
//! (§919). Tools declare the permissions they require; the host enforces
//! least-privilege before invocation (a WASM sandbox is the intended runtime
//! per §901, modelled here by the registry's permission gate).

use ckos_kernel::error::{KernelError, Result};
use std::collections::{HashMap, HashSet};

/// Plugin categories (§901).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Runtime,
    Memory,
    Graph,
    Tool,
    Ui,
    Workflow,
}

/// Metadata returned by every tool (§918).
#[derive(Debug, Clone)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    /// Permission tokens this tool requires, e.g. `filesystem.read` (§919).
    pub permissions: Vec<String>,
}

/// The unified tool interface (§918). All external integrations — filesystem,
/// git, docker, HTTP, … (§917) — are normalised behind this trait.
pub trait Tool: Send + Sync {
    /// Static metadata, including required permissions.
    fn metadata(&self) -> ToolMetadata;
    /// Validate input before execution; default accepts anything.
    fn validate(&self, _input: &str) -> Result<()> {
        Ok(())
    }
    /// Execute the tool against the input, returning its output.
    fn execute(&self, input: &str) -> Result<String>;
    /// Best-effort cancellation hook; default is a no-op.
    fn cancel(&self) {}
}

/// Registry of tools with a least-privilege permission gate (§917, §919).
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    /// Permission tokens the host has granted to the current context.
    granted: HashSet<String>,
}

impl ToolRegistry {
    /// Create an empty registry with no granted permissions (default-deny).
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant a permission token to the execution context.
    pub fn grant(&mut self, permission: impl Into<String>) {
        self.granted.insert(permission.into());
    }

    /// Register a tool under its metadata name.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.metadata().name;
        self.tools.insert(name, tool);
    }

    /// Names of all registered tools.
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.tools.keys().cloned().collect();
        v.sort();
        v
    }

    /// Invoke a tool by name. Fails if the tool is missing or any required
    /// permission has not been granted (§919).
    pub fn invoke(&self, name: &str, input: &str) -> Result<String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| KernelError::NotFound(format!("tool {name}")))?;
        let meta = tool.metadata();
        for perm in &meta.permissions {
            if !self.granted.contains(perm) {
                return Err(KernelError::PolicyDenied(format!(
                    "tool {name} requires permission {perm}"
                )));
            }
        }
        tool.validate(input)?;
        tool.execute(input)
    }
}

/// An uppercasing demo tool requiring no permissions — handy for tests.
pub struct UppercaseTool;

impl Tool for UppercaseTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "uppercase".into(),
            description: "uppercases its input".into(),
            permissions: vec![],
        }
    }
    fn execute(&self, input: &str) -> Result<String> {
        Ok(input.to_uppercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FileWriteTool;
    impl Tool for FileWriteTool {
        fn metadata(&self) -> ToolMetadata {
            ToolMetadata {
                name: "fs_write".into(),
                description: "writes a file".into(),
                permissions: vec!["filesystem.write".into()],
            }
        }
        fn execute(&self, input: &str) -> Result<String> {
            Ok(format!("wrote {input}"))
        }
    }

    #[test]
    fn invokes_permissionless_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(UppercaseTool));
        assert_eq!(reg.invoke("uppercase", "hi").unwrap(), "HI");
    }

    #[test]
    fn enforces_least_privilege() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(FileWriteTool));
        // Without the grant the call is denied.
        assert!(reg.invoke("fs_write", "a.txt").is_err());
        reg.grant("filesystem.write");
        assert_eq!(reg.invoke("fs_write", "a.txt").unwrap(), "wrote a.txt");
    }

    #[test]
    fn missing_tool_is_not_found() {
        let reg = ToolRegistry::new();
        assert!(matches!(
            reg.invoke("nope", "x"),
            Err(KernelError::NotFound(_))
        ));
    }
}
