//! PubNub を経由する bitbank Private Stream の購読。
//!
//! 仕様は [bitbank Private Stream](https://github.com/bitbankinc/bitbank-api-docs/blob/master/private-stream_JP.md)
//! と [PubNub Subscribe V2](https://www.pubnub.com/docs/sdks/rest-api/subscribe-v-2) を参照。

mod events;
#[cfg(test)]
mod tests;

pub use events::*;

use crate::{
    bitbank_private::BitbankPrivateApiClient, bitbank_structs::BitbankChannelAndTokenResponse,
};
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::VecDeque, fmt, time::Duration};

const PUBNUB_ENDPOINT: &str = "https://ps.pndsn.com";
const SUBSCRIBE_KEY: &str = "sub-c-ecebae8e-dd60-11e6-b6b1-02ee2ddab7fe";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(330);
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// 購読・通知受信のエラー。認証情報、URL、通知本文は保持しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitbankPrivateStreamError {
    /// bitbank の購読情報を取得できなかった。取得できた場合は取引所のエラーコードを含む。
    ChannelAndToken { exchange_code: Option<u64> },
    /// PubNub 用 HTTP クライアントを初期化できなかった。
    ClientBuild,
    /// PubNub との HTTP 通信に失敗した。
    Transport,
    /// PubNub が購読を拒否した。
    HttpStatus(u16),
    /// PubNub の購読レスポンスを解釈できなかった。
    InvalidResponse,
    /// bitbank の通知を解釈できなかった（未対応の通知種別を含む）。
    InvalidEvent,
}

impl fmt::Display for BitbankPrivateStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChannelAndToken { exchange_code } => {
                f.write_str("bitbank Private Stream channel and token request failed")?;
                if let Some(code) = exchange_code {
                    write!(f, " (bitbank code {code})")?;
                }
                Ok(())
            }
            Self::ClientBuild => f.write_str("PubNub HTTP client initialization failed"),
            Self::Transport => f.write_str("PubNub transport failed"),
            Self::HttpStatus(status) => write!(f, "PubNub subscribe failed (HTTP {status})"),
            Self::InvalidResponse => f.write_str("invalid PubNub subscribe response"),
            Self::InvalidEvent => {
                f.write_str("invalid or unsupported bitbank Private Stream event")
            }
        }
    }
}

impl std::error::Error for BitbankPrivateStreamError {}

/// 1つの bitbank Private Stream の購読。
///
/// `next_event` の呼び出し中にロングポーリングする。受信ループを中断し、
/// `close` を呼ぶかインスタンスを破棄すると購読が終了する。
pub struct BitbankPrivateStream {
    private_api: BitbankPrivateApiClient,
    http: Client,
    pubnub_endpoint: Url,
    credentials: BitbankChannelAndTokenResponse,
    cursor: Option<Cursor>,
    pending: VecDeque<PubNubMessage>,
    needs_token: bool,
}

#[derive(Deserialize)]
struct Cursor {
    #[serde(rename = "t")]
    timetoken: String,
    #[serde(rename = "r")]
    region: u32,
}

#[derive(Deserialize)]
struct SubscribeResponse {
    #[serde(rename = "t")]
    cursor: Cursor,
    #[serde(rename = "m")]
    messages: Vec<PubNubMessage>,
}

#[derive(Deserialize)]
struct PubNubMessage {
    #[serde(rename = "d")]
    payload: Value,
    #[serde(rename = "e", default)]
    kind: u8,
}

impl BitbankPrivateStream {
    /// bitbank から購読情報を取得し、PubNub の購読を開始する。
    pub async fn connect(
        private_api: &BitbankPrivateApiClient,
    ) -> Result<Self, BitbankPrivateStreamError> {
        Self::connect_to(private_api, Url::parse(PUBNUB_ENDPOINT).unwrap()).await
    }

    async fn connect_to(
        private_api: &BitbankPrivateApiClient,
        pubnub_endpoint: Url,
    ) -> Result<Self, BitbankPrivateStreamError> {
        let http = Client::builder()
            .connect_timeout(HANDSHAKE_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| BitbankPrivateStreamError::ClientBuild)?;
        let credentials = get_credentials(private_api).await?;
        let mut stream = Self {
            private_api: private_api.clone(),
            http,
            pubnub_endpoint,
            credentials,
            cursor: None,
            pending: VecDeque::new(),
            needs_token: false,
        };
        match stream.subscribe().await {
            Err(error) if is_auth_error(error) => {
                stream.refresh_token().await?;
                stream.subscribe().await?;
            }
            result => result?,
        }
        Ok(stream)
    }

    /// 次の通知を待つ。通信断・一時的な HTTP エラー時は最後の位置から再試行する。
    /// トークンを拒否された場合は取得し直し、同じチャンネルなら受信位置を引き継ぐ。
    /// 通知のデコードに失敗した場合はその通知のエラーを返し、次回は後続の通知を読む。
    ///
    /// 通信断中の通知が PubNub の保持範囲を超えることがある。残高や注文をローカルで
    /// 管理する場合は、必要に応じて REST API で最新の状態と照合する。
    pub async fn next_event(&mut self) -> Result<BitbankPrivateEvent, BitbankPrivateStreamError> {
        let mut retry_delay = INITIAL_RETRY_DELAY;
        let mut refreshed = false;
        loop {
            while let Some(message) = self.pending.pop_front() {
                // PubNub の通常メッセージに bitbank の通知が入る。
                if message.kind == 0 {
                    return serde_json::from_value(message.payload)
                        .map_err(|_| BitbankPrivateStreamError::InvalidEvent);
                }
            }

            if self.needs_token {
                self.refresh_token().await?;
                refreshed = true;
            }

            match self.subscribe().await {
                Ok(()) => {
                    retry_delay = INITIAL_RETRY_DELAY;
                    refreshed = false;
                }
                Err(error) if is_auth_error(error) => {
                    self.needs_token = true;
                    if refreshed {
                        return Err(error);
                    }
                }
                Err(error) if is_retryable(error) => {
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// 購読を終了し、受信バッファと HTTP クライアントを破棄する。
    pub fn close(self) {
        drop(self);
    }

    async fn refresh_token(&mut self) -> Result<(), BitbankPrivateStreamError> {
        let credentials = get_credentials(&self.private_api).await?;
        if credentials.pubnub_channel != self.credentials.pubnub_channel {
            self.cursor = None;
        }
        self.credentials = credentials;
        self.needs_token = false;
        Ok(())
    }

    async fn subscribe(&mut self) -> Result<(), BitbankPrivateStreamError> {
        let channel = self.credentials.pubnub_channel.as_str();
        let mut url = self.pubnub_endpoint.clone();
        url.path_segments_mut()
            .expect("PubNub endpoint is an HTTP URL")
            .extend(["v2", "subscribe", SUBSCRIBE_KEY, channel, "0"]);
        let mut request = self.http.get(url).query(&[
            ("uuid", channel),
            ("auth", self.credentials.pubnub_token.as_str()),
            (
                "pnsdk",
                concat!("bitbankutil_rs-Rust/", env!("CARGO_PKG_VERSION")),
            ),
        ]);
        request = match &self.cursor {
            Some(cursor) => request
                .timeout(SUBSCRIBE_TIMEOUT)
                .query(&[("tt", &cursor.timetoken)])
                .query(&[("tr", cursor.region)]),
            None => request.timeout(HANDSHAKE_TIMEOUT).query(&[("tt", "0")]),
        };
        let response = request
            .send()
            .await
            .map_err(|_| BitbankPrivateStreamError::Transport)?;
        if response.status() != StatusCode::OK {
            return Err(BitbankPrivateStreamError::HttpStatus(
                response.status().as_u16(),
            ));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| BitbankPrivateStreamError::Transport)?;
        let response: SubscribeResponse = serde_json::from_slice(&bytes)
            .map_err(|_| BitbankPrivateStreamError::InvalidResponse)?;
        if response.cursor.timetoken.is_empty()
            || !response
                .cursor
                .timetoken
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(BitbankPrivateStreamError::InvalidResponse);
        }
        self.cursor = Some(response.cursor);
        self.pending.extend(response.messages);
        Ok(())
    }
}

async fn get_credentials(
    private_api: &BitbankPrivateApiClient,
) -> Result<BitbankChannelAndTokenResponse, BitbankPrivateStreamError> {
    let credentials = tokio::time::timeout(HANDSHAKE_TIMEOUT, private_api.get_channel_and_token())
        .await
        .map_err(|_| BitbankPrivateStreamError::ChannelAndToken {
            exchange_code: None,
        })?
        .map_err(|error| BitbankPrivateStreamError::ChannelAndToken {
            exchange_code: crate::error_diagnostics::api(&error).1,
        })?;
    if credentials.pubnub_channel.is_empty() || credentials.pubnub_token.is_empty() {
        return Err(BitbankPrivateStreamError::ChannelAndToken {
            exchange_code: None,
        });
    }
    Ok(credentials)
}

fn is_auth_error(error: BitbankPrivateStreamError) -> bool {
    matches!(error, BitbankPrivateStreamError::HttpStatus(401 | 403))
}

fn is_retryable(error: BitbankPrivateStreamError) -> bool {
    matches!(
        error,
        BitbankPrivateStreamError::Transport
            | BitbankPrivateStreamError::HttpStatus(408 | 429 | 500..=599)
    )
}
