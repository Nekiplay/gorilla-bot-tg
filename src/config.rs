use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
    pub token: String,
    pub user_id: i64,
}
