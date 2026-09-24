use crate::adapters::{HarnessAdapter, KIMI_CODE};
use crate::capability::{Capability, Support};
use crate::home::Home;

pub struct KimiCode {
    home: Home,
}

impl KimiCode {
    pub fn new(home: Home) -> Self {
        Self { home }
    }

    pub fn home(&self) -> &Home {
        &self.home
    }
}

impl HarnessAdapter for KimiCode {
    fn name(&self) -> &str {
        KIMI_CODE
    }

    fn detect(&self) -> bool {
        self.home.kimi_dir().is_dir()
    }

    fn read_state(&self) -> anyhow::Result<Vec<Capability>> {
        Ok(Vec::new()) // Task 11 实现
    }

    fn supports(&self, _cap: &Capability) -> Support {
        Support::Native // Task 12 细化
    }
}
