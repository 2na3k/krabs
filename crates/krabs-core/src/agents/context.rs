use crate::providers::provider::Message;
use crate::session::session::SubturnResume;

/// Snapshot passed to the agent for one turn.
pub struct TurnInput {
    pub messages: Vec<Message>,
    pub subturn_resume: Option<SubturnResume>,
}

/// Persistent state of a multi-turn conversation.
///
/// Both CLI and server previously re-implemented this pattern: a typed
/// container for messages + resume metadata that survives across ephemeral
/// per-turn agent rebuilds. This is the canonical implementation.
pub struct ConversationContext {
    messages: Vec<Message>,
    subturn_resume: Option<SubturnResume>,
    turn_count: usize,
}

impl ConversationContext {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            subturn_resume: None,
            turn_count: 0,
        }
    }

    /// Restore from a previous session (resume scenario).
    pub fn from_history(messages: Vec<Message>, subturn_resume: Option<SubturnResume>) -> Self {
        Self {
            messages,
            subturn_resume,
            turn_count: 0,
        }
    }

    /// Append a user message and return a snapshot for the agent turn.
    ///
    /// The snapshot is a clone — the agent may mutate it internally
    /// (insert system prompt, append assistant messages), but the canonical
    /// state only updates via [`complete_turn`](Self::complete_turn).
    pub fn begin_turn(&mut self, user_message: &str) -> TurnInput {
        self.messages.push(Message::user(user_message));
        TurnInput {
            messages: self.messages.clone(),
            subturn_resume: self.subturn_resume.take(),
        }
    }

    /// Update canonical messages with the final result from the agent.
    pub fn complete_turn(&mut self, final_messages: Vec<Message>) {
        self.messages = final_messages;
        self.turn_count += 1;
    }

    /// Read-only access to current messages.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Number of completed turns.
    pub fn turn_count(&self) -> usize {
        self.turn_count
    }
}

impl Default for ConversationContext {
    fn default() -> Self {
        Self::new()
    }
}
