use super::ComposeExec;
use crate::error::TotsukactlError;
use async_trait::async_trait;
use std::sync::Mutex;

#[derive(Default)]
pub struct MockCompose {
    pub running: Mutex<bool>,
    pub image: Mutex<String>,
    pub calls: Mutex<Vec<String>>,
    pub fail_docker_info: Mutex<bool>,
}

impl MockCompose {
    pub fn with_image(image: &str) -> Self {
        Self {
            image: Mutex::new(image.into()),
            ..Default::default()
        }
    }

    pub fn record(&self, c: &str) {
        self.calls.lock().unwrap().push(c.into());
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ComposeExec for MockCompose {
    async fn docker_info(&self) -> Result<(), TotsukactlError> {
        self.record("docker_info");
        if *self.fail_docker_info.lock().unwrap() {
            Err(TotsukactlError::Compose("docker daemon down".into()))
        } else {
            Ok(())
        }
    }

    async fn compose_version(&self) -> Result<(), TotsukactlError> {
        self.record("compose_version");
        Ok(())
    }

    async fn ps_running(&self, service: &str) -> Result<bool, TotsukactlError> {
        self.record(&format!("ps_running:{service}"));
        Ok(*self.running.lock().unwrap())
    }

    async fn up_detached(&self, service: &str, recreate: bool) -> Result<(), TotsukactlError> {
        self.record(&format!("up_detached:{service}:{recreate}"));
        *self.running.lock().unwrap() = true;
        Ok(())
    }

    async fn stop(&self, service: &str) -> Result<(), TotsukactlError> {
        self.record(&format!("stop:{service}"));
        *self.running.lock().unwrap() = false;
        Ok(())
    }

    async fn inspect_image(&self, container: &str) -> Result<String, TotsukactlError> {
        self.record(&format!("inspect_image:{container}"));
        Ok(self.image.lock().unwrap().clone())
    }

    async fn logs_tail(&self, service: &str, _n: u32) -> Result<String, TotsukactlError> {
        self.record(&format!("logs_tail:{service}"));
        Ok(String::new())
    }
}
