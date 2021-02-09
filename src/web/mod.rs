//! Web authentication, handlers, and middleware
pub mod extractors;
pub mod handlers;
pub mod middleware;
#[cfg(test)]
mod test;
mod user_agent;

// Known DockerFlow commands for Ops callbacks
pub const DOCKER_FLOW_ENDPOINTS: [&str; 4] = [
    "/__heartbeat__",
    "/__lbheartbeat__",
    "/__version__",
    "/__error__",
];
