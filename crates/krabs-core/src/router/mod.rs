use crate::config::config::RouterConfig;
use async_trait::async_trait;
use regex::Regex;

/// Which execution strategy the agent should use for a given task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    /// Single reactive loop — current default behaviour.
    Reactive,
    /// Plan first, then execute each step (multi-step structured tasks).
    Planned,
    /// Breadth-first exploration — no fixed goal, wide discovery.
    Explore,
}

impl RouteDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reactive => "reactive",
            Self::Planned => "planned",
            Self::Explore => "explore",
        }
    }
}

/// Classifies an incoming task string into a [`RouteDecision`].
#[async_trait]
pub trait TaskRouter: Send + Sync {
    async fn route(&self, task: &str) -> RouteDecision;
}

// ---------------------------------------------------------------------------
// RulesRouter — pure regex, zero LLM cost
// ---------------------------------------------------------------------------

pub struct RulesRouter {
    rules: Vec<(Regex, RouteDecision)>,
    fallback: RouteDecision,
}

impl RulesRouter {
    pub fn from_config(config: &RouterConfig) -> Self {
        let rules = config
            .rules
            .iter()
            .filter_map(|r| {
                let re = Regex::new(&format!("(?i){}", r.pattern)).ok()?;
                Some((re, parse_decision(&r.target)))
            })
            .collect();

        Self {
            rules,
            fallback: parse_decision(&config.fallback),
        }
    }
}

#[async_trait]
impl TaskRouter for RulesRouter {
    async fn route(&self, task: &str) -> RouteDecision {
        for (re, decision) in &self.rules {
            if re.is_match(task) {
                return decision.clone();
            }
        }
        self.fallback.clone()
    }
}

// ---------------------------------------------------------------------------
// FixedRouter — always returns the same decision (used for forced modes)
// ---------------------------------------------------------------------------

pub struct FixedRouter(pub RouteDecision);

#[async_trait]
impl TaskRouter for FixedRouter {
    async fn route(&self, _task: &str) -> RouteDecision {
        self.0.clone()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn parse_decision(s: &str) -> RouteDecision {
    match s.trim().to_lowercase().as_str() {
        "planned" => RouteDecision::Planned,
        "explore" => RouteDecision::Explore,
        _ => RouteDecision::Reactive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::config::{RouterConfig, RouterRule};

    fn cfg(rules: Vec<(&str, &str)>, fallback: &str) -> RouterConfig {
        RouterConfig {
            mode: "auto".to_string(),
            classifier: "rules".to_string(),
            rules: rules
                .into_iter()
                .map(|(p, t)| RouterRule {
                    pattern: p.to_string(),
                    target: t.to_string(),
                })
                .collect(),
            fallback: fallback.to_string(),
        }
    }

    #[tokio::test]
    async fn matches_first_rule() {
        let router = RulesRouter::from_config(&cfg(
            vec![("explore|research", "explore"), ("build|create", "planned")],
            "reactive",
        ));
        assert_eq!(
            router.route("explore the codebase").await,
            RouteDecision::Explore
        );
        assert_eq!(
            router.route("build a new API").await,
            RouteDecision::Planned
        );
    }

    #[tokio::test]
    async fn case_insensitive() {
        let router = RulesRouter::from_config(&cfg(vec![("EXPLORE", "explore")], "reactive"));
        assert_eq!(router.route("explore things").await, RouteDecision::Explore);
        assert_eq!(router.route("EXPLORE things").await, RouteDecision::Explore);
    }

    #[tokio::test]
    async fn falls_back_when_no_match() {
        let router = RulesRouter::from_config(&cfg(vec![("explore", "explore")], "planned"));
        assert_eq!(
            router.route("write some code").await,
            RouteDecision::Planned
        );
    }

    #[tokio::test]
    async fn fixed_router_always_same() {
        let router = FixedRouter(RouteDecision::Explore);
        assert_eq!(router.route("anything").await, RouteDecision::Explore);
        assert_eq!(router.route("something else").await, RouteDecision::Explore);
    }

    #[test]
    fn parse_decision_variants() {
        assert_eq!(parse_decision("planned"), RouteDecision::Planned);
        assert_eq!(parse_decision("explore"), RouteDecision::Explore);
        assert_eq!(parse_decision("reactive"), RouteDecision::Reactive);
        assert_eq!(parse_decision("unknown"), RouteDecision::Reactive);
        assert_eq!(parse_decision("  PLANNED  "), RouteDecision::Planned);
    }

    #[test]
    fn route_decision_as_str() {
        assert_eq!(RouteDecision::Reactive.as_str(), "reactive");
        assert_eq!(RouteDecision::Planned.as_str(), "planned");
        assert_eq!(RouteDecision::Explore.as_str(), "explore");
    }
}
