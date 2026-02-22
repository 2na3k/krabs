use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn set(&self, key: &str, value: &str) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn delete(&self, key: &str) -> Result<()>;
    async fn keys(&self) -> Result<Vec<String>>;
}

pub struct InMemoryStore {
    data: RwLock<HashMap<String, String>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    async fn set(&self, key: &str, value: &str) -> Result<()> {
        self.data
            .write()
            .map_err(|e| anyhow::anyhow!("memory lock poisoned: {}", e))?
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .data
            .read()
            .map_err(|e| anyhow::anyhow!("memory lock poisoned: {}", e))?
            .get(key)
            .cloned())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.data
            .write()
            .map_err(|e| anyhow::anyhow!("memory lock poisoned: {}", e))?
            .remove(key);
        Ok(())
    }

    async fn keys(&self) -> Result<Vec<String>> {
        let mut keys: Vec<String> = self
            .data
            .read()
            .map_err(|e| anyhow::anyhow!("memory lock poisoned: {}", e))?
            .keys()
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }
}
