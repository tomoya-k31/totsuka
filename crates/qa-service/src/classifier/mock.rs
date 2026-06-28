use super::*;
use std::sync::Mutex;

pub struct MockClassifier {
    response: Mutex<ClassifyResponse>,
}

impl MockClassifier {
    pub fn new(response: ClassifyResponse) -> Self { Self { response: Mutex::new(response) } }
    pub fn set_response(&self, r: ClassifyResponse) { *self.response.lock().unwrap() = r; }
}

#[async_trait]
impl Classifier for MockClassifier {
    async fn classify(&self, _req: ClassifyRequest) -> Result<ClassifyResponse, QaError> {
        Ok(self.response.lock().unwrap().clone())
    }
    fn provider(&self) -> &str { "mock" }
    fn model(&self) -> &str { "mock-model" }
}
