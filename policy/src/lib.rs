//! # CKOS Policy
//!
//! Two-layer authorization (§929): role-based (RBAC) plus attribute-based
//! (ABAC). Permissions are expressed as scoped tokens like `graph.read` or
//! `filesystem.write` (§908, §919) and the engine defaults to **deny**, keeping
//! least-privilege the norm (§919).

use ckos_kernel::error::{KernelError, Result};
use std::collections::{HashMap, HashSet};

pub mod identity;
pub use identity::{Identity, IdentityProvider, StaticTokenProvider};

/// A request to perform `action` on `resource` by a `subject` with attributes.
#[derive(Debug, Clone)]
pub struct AccessRequest {
    /// The acting principal (user or agent id).
    pub subject: String,
    /// Roles held by the subject (RBAC).
    pub roles: Vec<String>,
    /// Permission token requested, e.g. `graph.write` (§919).
    pub action: String,
    /// Attribute key/value pairs for ABAC rules, e.g. `env=prod`.
    pub attributes: HashMap<String, String>,
}

/// An attribute-based rule (§929): when the attribute matches, the action is
/// granted (or, if `deny`, explicitly forbidden — deny wins).
#[derive(Debug, Clone)]
pub struct AbacRule {
    pub action: String,
    pub attribute_key: String,
    pub attribute_value: String,
    pub deny: bool,
}

/// RBAC + ABAC engine.
#[derive(Default)]
pub struct PolicyEngine {
    /// Role -> granted permission tokens.
    role_permissions: HashMap<String, HashSet<String>>,
    /// Attribute-based overrides evaluated after RBAC.
    abac_rules: Vec<AbacRule>,
}

impl PolicyEngine {
    /// Create an empty engine (default-deny).
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant a permission token to a role (RBAC).
    pub fn grant(&mut self, role: impl Into<String>, action: impl Into<String>) {
        self.role_permissions
            .entry(role.into())
            .or_default()
            .insert(action.into());
    }

    /// Add an ABAC rule.
    pub fn add_rule(&mut self, rule: AbacRule) {
        self.abac_rules.push(rule);
    }

    /// Whether `action` is permitted, honouring `.*` wildcards on the trailing
    /// segment (e.g. a role granted `graph.*` covers `graph.read`).
    fn rbac_allows(&self, roles: &[String], action: &str) -> bool {
        roles.iter().any(|role| {
            self.role_permissions.get(role).is_some_and(|perms| {
                perms.contains(action)
                    || perms.iter().any(|p| {
                        p.strip_suffix('*')
                            .is_some_and(|prefix| action.starts_with(prefix))
                    })
            })
        })
    }

    /// Decide a request. Explicit ABAC deny always wins; otherwise an ABAC
    /// grant or an RBAC grant permits the action; everything else is denied.
    pub fn evaluate(&self, req: &AccessRequest) -> Result<()> {
        // 1. Explicit deny rules take precedence.
        for rule in &self.abac_rules {
            if rule.deny
                && rule.action == req.action
                && req.attributes.get(&rule.attribute_key) == Some(&rule.attribute_value)
            {
                return Err(KernelError::PolicyDenied(format!(
                    "{} denied for {}={}",
                    req.action, rule.attribute_key, rule.attribute_value
                )));
            }
        }

        // 2. ABAC allow rules.
        let abac_allow = self.abac_rules.iter().any(|rule| {
            !rule.deny
                && rule.action == req.action
                && req.attributes.get(&rule.attribute_key) == Some(&rule.attribute_value)
        });

        // 3. RBAC.
        if abac_allow || self.rbac_allows(&req.roles, &req.action) {
            Ok(())
        } else {
            Err(KernelError::PolicyDenied(format!(
                "{} not permitted for {}",
                req.action, req.subject
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(roles: &[&str], action: &str, attrs: &[(&str, &str)]) -> AccessRequest {
        AccessRequest {
            subject: "agent-1".into(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
            action: action.into(),
            attributes: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn defaults_to_deny() {
        let p = PolicyEngine::new();
        assert!(p.evaluate(&req(&["guest"], "graph.write", &[])).is_err());
    }

    #[test]
    fn rbac_wildcard_grants() {
        let mut p = PolicyEngine::new();
        p.grant("editor", "graph.*");
        assert!(p.evaluate(&req(&["editor"], "graph.read", &[])).is_ok());
        assert!(p.evaluate(&req(&["editor"], "graph.write", &[])).is_ok());
        assert!(p.evaluate(&req(&["editor"], "docker.run", &[])).is_err());
    }

    #[test]
    fn abac_deny_overrides_rbac_grant() {
        let mut p = PolicyEngine::new();
        p.grant("admin", "docker.run");
        p.add_rule(AbacRule {
            action: "docker.run".into(),
            attribute_key: "env".into(),
            attribute_value: "prod".into(),
            deny: true,
        });
        assert!(p.evaluate(&req(&["admin"], "docker.run", &[])).is_ok());
        assert!(p
            .evaluate(&req(&["admin"], "docker.run", &[("env", "prod")]))
            .is_err());
    }
}
