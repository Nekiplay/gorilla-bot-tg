use std::{fs, io::Read};

use serde_json::from_str;
use vkontakte::{
    VkError,
    dispatch::TextRule,
    framework::{User, UserBuilder},
};

use crate::any_message_rule::AnyMessageRule;
use crate::config::Config;

pub mod any_message_rule;
pub mod config;

#[tokio::main]
async fn main() -> Result<(), VkError> {
    let lines = fs::read_to_string("config.json").expect("Failed to read config");
    let config: Config = from_str(&lines)?;

    let mut user = UserBuilder::new(config.token)
        .user_id(config.user_id)
        .build()
        .await?;

    register_handlers(&mut user);
    user.run().await
}
fn register_handlers(user: &mut User) {
    user.on()
        .message(Box::new(AnyMessageRule))
        .handle(|msg, _| async move {
            if msg.from_id == -171493284 {
                msg.answer("Обнаружено сообщения от Gorrila Bot").await?;
            }
            Ok(None)
        });
}
