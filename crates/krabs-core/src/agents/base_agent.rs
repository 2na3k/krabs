/// Built-in agent profiles embedded at compile time.
///
/// Each variant corresponds to a markdown file in `base_agent/`.
/// The markdown body forms the agent's role-specific system prompt extension —
/// it is appended after the immutable SOUL + SYSTEM_PROMPT base.
///
/// # Example
/// ```no_run
/// use krabs_core::agents::{BaseAgent, KrabsAgentBuilder};
///
/// let agent = KrabsAgentBuilder::new(config, provider)
///     .system_prompt(BaseAgent::Planner.system_prompt())
///     .build();
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseAgent {
    Planner,
    FrontendDeveloper,
    Explorer,
}

impl BaseAgent {
    /// Returns the compile-time embedded system prompt for this agent profile.
    pub fn system_prompt(self) -> &'static str {
        match self {
            Self::Planner => include_str!("base_agent/planner.md"),
            Self::FrontendDeveloper => include_str!("base_agent/frontend_developer.md"),
            Self::Explorer => include_str!("base_agent/explorer.md"),
        }
    }

    /// Returns the canonical name for this agent profile.
    pub fn name(self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::FrontendDeveloper => "frontend_developer",
            Self::Explorer => "explorer",
        }
    }

    /// Returns all built-in agent profiles.
    pub fn all() -> &'static [Self] {
        &[Self::Planner, Self::FrontendDeveloper, Self::Explorer]
    }
}

impl std::fmt::Display for BaseAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
