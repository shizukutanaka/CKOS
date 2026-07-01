//! Shared permission-token matching (§919), used by both `policy` (RBAC/ABAC,
//! §929) and `plugins` (tool permission gate, §917) so a granted token like
//! `graph.*` is honoured identically everywhere a permission check happens.

/// Whether `required` is covered by `granted` — an exact match, or `granted`
/// ends in `*` and `required` starts with the prefix before it (e.g. `graph.*`
/// covers `graph.read`).
pub fn permission_matches(granted: &str, required: &str) -> bool {
    granted == required
        || granted
            .strip_suffix('*')
            .is_some_and(|prefix| required.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(permission_matches("graph.read", "graph.read"));
        assert!(!permission_matches("graph.read", "graph.write"));
    }

    #[test]
    fn wildcard_covers_prefix() {
        assert!(permission_matches("graph.*", "graph.read"));
        assert!(permission_matches("graph.*", "graph.write"));
        assert!(!permission_matches("graph.*", "docker.run"));
        // A wildcard must not match an unrelated string that merely shares a
        // substring — "graph" without the separator does not cover "graphite.x".
        assert!(!permission_matches("graph.*", "graphite.x"));
    }
}
