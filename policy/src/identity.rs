//! Enterprise identity (§928).
//!
//! An [`IdentityProvider`] turns an opaque credential (a bearer/OIDC token) into
//! a verified [`Identity`] — subject, roles and attributes — which feeds the
//! RBAC/ABAC [`PolicyEngine`](crate::PolicyEngine) of §929. Real backends
//! (OAuth2, OpenID Connect, SAML, LDAP, Active Directory) implement the same
//! trait; [`StaticTokenProvider`] is an in-memory stand-in for tests and local
//! development.

use crate::AccessRequest;
use ckos_kernel::error::{KernelError, Result};
use std::collections::HashMap;

/// A verified principal (§928).
#[derive(Debug, Clone, Default)]
pub struct Identity {
    /// Stable subject id (e.g. the OIDC `sub` claim).
    pub subject: String,
    /// Roles claimed by the token (RBAC, §929).
    pub roles: Vec<String>,
    /// Attribute claims for ABAC, e.g. `env=prod`, `dept=research`.
    pub attributes: HashMap<String, String>,
}

impl Identity {
    /// Create an identity for a subject.
    pub fn new(subject: impl Into<String>) -> Self {
        Identity {
            subject: subject.into(),
            roles: Vec::new(),
            attributes: HashMap::new(),
        }
    }

    /// Builder: add a role.
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.roles.push(role.into());
        self
    }

    /// Builder: add an attribute.
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Build an [`AccessRequest`] for `action` from this identity, so the policy
    /// engine can authorize it (§929).
    pub fn request(&self, action: impl Into<String>) -> AccessRequest {
        AccessRequest {
            subject: self.subject.clone(),
            roles: self.roles.clone(),
            action: action.into(),
            attributes: self.attributes.clone(),
        }
    }
}

/// Validates a credential and returns the principal it represents (§928).
pub trait IdentityProvider: Send + Sync {
    /// Authenticate a token. Returns the verified identity or a denial.
    fn authenticate(&self, token: &str) -> Result<Identity>;
}

/// An in-memory token → identity map standing in for an OIDC/LDAP directory.
#[derive(Default)]
pub struct StaticTokenProvider {
    tokens: HashMap<String, Identity>,
}

impl StaticTokenProvider {
    /// Create an empty provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a token that resolves to `identity`.
    pub fn add_token(&mut self, token: impl Into<String>, identity: Identity) {
        self.tokens.insert(token.into(), identity);
    }
}

impl IdentityProvider for StaticTokenProvider {
    fn authenticate(&self, token: &str) -> Result<Identity> {
        self.tokens
            .get(token)
            .cloned()
            .ok_or_else(|| KernelError::PolicyDenied("invalid or unknown token".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PolicyEngine;

    #[test]
    fn authenticates_then_authorizes() {
        let mut provider = StaticTokenProvider::new();
        provider.add_token(
            "tok-alice",
            Identity::new("alice")
                .with_role("editor")
                .with_attribute("env", "staging"),
        );

        let mut policy = PolicyEngine::new();
        policy.grant("editor", "graph.write");

        // Valid token → identity → authorized action.
        let id = provider.authenticate("tok-alice").unwrap();
        assert_eq!(id.subject, "alice");
        assert!(policy.evaluate(&id.request("graph.write")).is_ok());
        // An action the role lacks is denied.
        assert!(policy.evaluate(&id.request("docker.run")).is_err());
    }

    #[test]
    fn unknown_token_is_denied() {
        let provider = StaticTokenProvider::new();
        assert!(matches!(
            provider.authenticate("nope"),
            Err(KernelError::PolicyDenied(_))
        ));
    }
}
