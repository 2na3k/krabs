use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct PermissionGuard {
    allow_list: Option<HashSet<String>>,
    deny_list: HashSet<String>,
}

impl PermissionGuard {
    pub fn new() -> Self {
        Self {
            allow_list: None,
            deny_list: HashSet::new(),
        }
    }
    pub fn allow_only(tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allow_list: Some(tools.into_iter().map(|s| s.into()).collect()),
            deny_list: HashSet::new(),
        }
    }
    pub fn deny(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.deny_list.extend(tools.into_iter().map(|s| s.into()));
        self
    }
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        if self.deny_list.contains(tool_name) {
            return false;
        }
        if let Some(ref allow) = self.allow_list {
            return allow.contains(tool_name);
        }
        true
    }
}

impl Default for PermissionGuard {
    fn default() -> Self {
        Self::new()
    }
}
