#[allow(dead_code, non_snake_case)]
#[derive(serde::Deserialize, Debug, Clone)]
pub struct BitbankWebSocketMessage {
    pub message: BitbankWebSocketContent,
    pub room_name: String,
}

#[allow(dead_code, non_snake_case)]
#[derive(serde::Deserialize, Debug, Clone)]
pub struct BitbankWebSocketContent {
    //pub pid: Number, // Not necessary exist
    pub data: serde_json::Value,
}
