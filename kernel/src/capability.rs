//! Capabilities (§911) — the vocabulary the registry matches against.
//!
//! Agents are discovered by *capability*, not by name (§910), so a workflow can
//! swap one planning agent for another without changing its definition.

use std::fmt;
use std::str::FromStr;

/// A capability an agent may advertise (§911).
///
/// The list mirrors the spec but stays open via [`Capability::Custom`] so
/// third-party agents can declare domain capabilities (e.g. `robotics`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    Planning,
    Reasoning,
    Coding,
    Translation,
    Embedding,
    Retrieval,
    Verification,
    Simulation,
    Vision,
    Speech,
    Robotics,
    Finance,
    Medical,
    Legal,
    /// Escape hatch for capabilities not enumerated above.
    Custom(String),
}

impl Capability {
    /// Canonical lowercase token used in manifests and the registry.
    pub fn as_token(&self) -> &str {
        match self {
            Capability::Planning => "planning",
            Capability::Reasoning => "reasoning",
            Capability::Coding => "coding",
            Capability::Translation => "translation",
            Capability::Embedding => "embedding",
            Capability::Retrieval => "retrieval",
            Capability::Verification => "verification",
            Capability::Simulation => "simulation",
            Capability::Vision => "vision",
            Capability::Speech => "speech",
            Capability::Robotics => "robotics",
            Capability::Finance => "finance",
            Capability::Medical => "medical",
            Capability::Legal => "legal",
            Capability::Custom(s) => s,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_token())
    }
}

impl FromStr for Capability {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let cap = match s.trim().to_ascii_lowercase().as_str() {
            "planning" => Capability::Planning,
            "reasoning" => Capability::Reasoning,
            "coding" => Capability::Coding,
            "translation" => Capability::Translation,
            "embedding" => Capability::Embedding,
            "retrieval" => Capability::Retrieval,
            "verification" => Capability::Verification,
            "simulation" => Capability::Simulation,
            "vision" => Capability::Vision,
            "speech" => Capability::Speech,
            "robotics" => Capability::Robotics,
            "finance" => Capability::Finance,
            "medical" => Capability::Medical,
            "legal" => Capability::Legal,
            other => Capability::Custom(other.to_string()),
        };
        Ok(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_and_custom() {
        assert_eq!("planning".parse::<Capability>().unwrap(), Capability::Planning);
        assert_eq!(
            "weather".parse::<Capability>().unwrap(),
            Capability::Custom("weather".into())
        );
    }

    #[test]
    fn token_round_trips() {
        let c = Capability::Verification;
        assert_eq!(c.as_token().parse::<Capability>().unwrap(), c);
    }
}
