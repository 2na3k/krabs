use crate::tools::tool::ToolDef;

pub struct SystemPromptBuilder {
    base: String,
    sections: Vec<String>,
}

impl SystemPromptBuilder {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            sections: Vec::new(),
        }
    }
    pub fn with_tools(mut self, tools: &[ToolDef]) -> Self {
        if tools.is_empty() {
            return self;
        }
        let tool_list = tools
            .iter()
            .map(|t| format!("- `{}`: {}", t.name, t.description))
            .collect::<Vec<_>>()
            .join("\n");
        self.sections
            .push(format!("## Available Tools\n{}", tool_list));
        self
    }
    pub fn with_section(mut self, title: &str, content: &str) -> Self {
        self.sections.push(format!("## {}\n{}", title, content));
        self
    }
    pub fn build(self) -> String {
        if self.sections.is_empty() {
            return self.base;
        }
        format!("{}\n\n{}", self.base, self.sections.join("\n\n"))
    }
}
