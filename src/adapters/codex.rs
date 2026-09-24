use crate::adapters::{CODEX, HarnessAdapter};
use crate::capability::{Capability, Support};
use crate::home::Home;

pub struct Codex {
    home: Home,
}

impl Codex {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }
}

impl HarnessAdapter for Codex {
    fn name(&self) -> &str {
        CODEX
    }

    fn detect(&self) -> bool {
        self.home.codex_dir().is_dir()
    }

    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        Ok(Vec::new()) // Task 10 实现
    }

    fn supports(&self, _cap: &Capability) -> Support {
        Support::Native // Task 12 细化
    }
}
