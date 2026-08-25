#!/usr/bin/env bash
# Structural checks on the deployment manifests (§931/§932).
#
# These files were shipped marked ✅ while being undeployable: the Kubernetes
# Deployment ran `args: ["help"]`, which exits 0, so the pod sat in
# CrashLoopBackOff permanently — with no Service in front of it and an HPA
# scaling the CPU of a workload that never ran. `docker-compose.yml`
# meanwhile started Neo4j, Qdrant and Redis, none of which anything connects
# to, since CKOS has zero external dependencies by design.
#
# Nothing caught that, because YAML that no test reads is just prose. This
# asserts the handful of properties that make the manifests actually work.
# It is not a substitute for `kubectl apply` against a real cluster — no
# cluster or container runtime is available in this environment — so it checks
# structure, not behaviour, and says so.
#
# Run standalone, or via ./scripts/check.sh which includes it.
set -euo pipefail
cd "$(dirname "$0")/.."

exec python3 - <<'PY'
import sys

try:
    import yaml
except ImportError:
    # std-only is a Rust policy, not a reason to hard-require a Python
    # package. Skip loudly rather than failing a contributor's commit.
    print("check-deploy: PyYAML not installed — skipping manifest checks", file=sys.stderr)
    sys.exit(0)

failures = []


def check(cond, msg):
    if not cond:
        failures.append(msg)


def load(path, multi=False):
    """Parse, reporting a syntax error as a plain failure. A traceback here
    reads like the checker is broken when in fact the manifest is."""
    try:
        with open(path) as f:
            return [d for d in yaml.safe_load_all(f) if d] if multi else yaml.safe_load(f)
    except yaml.YAMLError as e:
        print(f"{path} is not valid YAML:\n  {e}", file=sys.stderr)
        sys.exit(1)


docs = load("deploy/k8s/ckos.yaml", multi=True)
by_kind = {d["kind"]: d for d in docs}

check("Service" in by_kind, "k8s: a Deployment with no Service is unreachable")
check("Deployment" in by_kind, "k8s: no Deployment")

if "Deployment" in by_kind:
    dep = by_kind["Deployment"]
    c = dep["spec"]["template"]["spec"]["containers"][0]
    args = c.get("args", [])
    # The defect this file exists for: a Deployment must host a process that
    # keeps running. Every CKOS subcommand except `serve` exits immediately.
    check(
        args[:1] == ["serve"],
        f"k8s: a Deployment must run a long-lived process; `ckos {' '.join(args[:1]) or '<none>'}` "
        "exits immediately and will CrashLoopBackOff",
    )
    check("0.0.0.0" in args, "k8s: must bind 0.0.0.0; 127.0.0.1 is unreachable from outside the pod")
    check("readinessProbe" in c and "livenessProbe" in c,
          "k8s: probes required, or a server that failed to bind still takes traffic")

    ports = c.get("ports", [])
    if ports and "--port" in args:
        declared = str(ports[0]["containerPort"])
        check(args[args.index("--port") + 1] == declared,
              "k8s: containerPort must match the --port the server binds")

    mounts = c.get("volumeMounts", [])
    vols = {v["name"] for v in dep["spec"]["template"]["spec"].get("volumes", [])}
    for m in mounts:
        check(m["name"] in vols, f"k8s: volumeMount {m['name']} has no matching volume")
    if mounts and "--session-root" in args:
        check(args[args.index("--session-root") + 1] == mounts[0]["mountPath"],
              "k8s: --session-root must point at the mounted path, or sessions vanish")

if "Service" in by_kind and "Deployment" in by_kind:
    svc, dep = by_kind["Service"], by_kind["Deployment"]
    # The gateway has no TLS and no auth (see the ckos_web crate docs).
    check(svc["spec"].get("type") == "ClusterIP",
          "k8s: an unauthenticated gateway must not be exposed beyond ClusterIP")
    check(svc["spec"]["selector"] == dep["spec"]["selector"]["matchLabels"],
          "k8s: Service selector does not match the pod labels — it would select nothing")

comp = load("docker-compose.yml")
services = list(comp.get("services", {}))
check(services == ["ckos"],
      f"compose: only services something actually connects to belong here; found {services}. "
      "CKOS has zero external dependencies — a database service here implies an "
      "integration that does not exist")
ckos = comp["services"].get("ckos", {})
check(all(p.startswith("127.0.0.1:") for p in ckos.get("ports", [])),
      "compose: publish the unauthenticated gateway on loopback only")

if failures:
    print("deployment manifests are not deployable:", file=sys.stderr)
    for f in failures:
        print(f"  {f}", file=sys.stderr)
    sys.exit(1)

print(f"deploy manifests: {len(docs)} k8s document(s) + compose, structurally sound.")
PY
