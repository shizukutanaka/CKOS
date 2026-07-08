//! The embedded browser dashboard (v2.8 Graph Explorer / Runtime Monitor
//! groundwork, §902) — a single self-contained HTML file with inline CSS and
//! vanilla JavaScript. No build step, no CDN, no external assets: it is
//! baked into the `ckos-web` binary via [`include_str!`] and works fully
//! offline, matching the workspace's dependency-free guarantee.

/// The dashboard page, served at `GET /`.
pub const PAGE: &str = include_str!("dashboard.html");
