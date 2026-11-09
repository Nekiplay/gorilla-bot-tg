use async_trait::async_trait;
use serde_json::Value;
use vkontakte::dispatch::{Rule, RuleResult};
pub struct AnyMessageRule;

#[async_trait]
impl Rule<Value> for AnyMessageRule {
    async fn check(&self, _event: &Value) -> RuleResult {
        // Всегда пропускать, перехватывать любое сообщение
        RuleResult::Pass
    }

    fn description(&self) -> String {
        "AnyMessageRule".to_string()
    }
}
