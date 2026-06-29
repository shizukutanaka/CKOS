//! `ckos` — the CKOS command-line interface (§902, §906).
//!
//! A deliberately small surface that demonstrates the kernel end-to-end without
//! any external runtime: plan an intent into a DAG, show the resulting tasks and
//! the agents that would be selected by capability.

use ckos_sdk::prelude::*;
use std::process::ExitCode;

const HELP: &str = "\
ckos — Cognitive Kernel OS

USAGE:
    ckos <COMMAND> [ARGS]

COMMANDS:
    plan <intent...>    Decompose an intent into a workflow DAG
    capabilities        List the built-in capability vocabulary
    version             Print the CKOS version
    help                Show this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("plan") => cmd_plan(&args[1..]),
        Some("capabilities") => cmd_capabilities(),
        Some("version") => {
            println!("ckos {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help") | None => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            print!("{HELP}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_plan(rest: &[String]) -> ExitCode {
    if rest.is_empty() {
        eprintln!("error: `plan` needs an intent, e.g. `ckos plan research transformers`");
        return ExitCode::FAILURE;
    }
    let intent = rest.join(" ");
    let dag = HeuristicPlanner::new().plan(&intent);

    // Register one demo agent per capability so we can show discovery.
    let mut registry = CapabilityRegistry::new();
    for cap in [
        Capability::Retrieval,
        Capability::Embedding,
        Capability::Reasoning,
        Capability::Verification,
    ] {
        registry.register(AgentManifest::new(format!("{cap}-agent"), cap));
    }

    println!("intent : {intent}");
    println!("workflow: {} ({} step(s))", dag.name(), dag.len());

    match dag.topological_order() {
        Some(order) => {
            println!("\nexecution order:");
            for (i, step) in order.iter().enumerate() {
                if let Some(task) = dag.task(*step) {
                    let agents = registry.discover(&task.capability).len();
                    println!(
                        "  {}. [{}] {}  (agents available: {})",
                        i + 1,
                        task.capability,
                        task.description,
                        agents
                    );
                }
            }
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("error: workflow contains a cycle and cannot be scheduled");
            ExitCode::FAILURE
        }
    }
}

fn cmd_capabilities() -> ExitCode {
    let caps = [
        "planning",
        "reasoning",
        "coding",
        "translation",
        "embedding",
        "retrieval",
        "verification",
        "simulation",
        "vision",
        "speech",
        "robotics",
        "finance",
        "medical",
        "legal",
    ];
    println!("built-in capabilities (§911):");
    for c in caps {
        println!("  - {c}");
    }
    ExitCode::SUCCESS
}
