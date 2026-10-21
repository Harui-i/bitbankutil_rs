//! ローカルHTTPサーバーで認証付きREST照会の実際のリクエストを検証する。
use super::*;
use crate::{
    account_domain::{HistoryOrder, OrderStatus, TradeHistoryQuery},
    order_domain::OrderId,
    trading_venue::{BitbankTradingVenue, TradingVenue, TradingVenueError},
};
use crypto_botters::generic_api_client::http::RequestConfig;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

fn mock_venue(response: Value) -> (BitbankTradingVenue, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "mock server received no request");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0, "request ended early");
            bytes.extend_from_slice(&buf[..n]);
            if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..pos]).to_lowercase();
                let len: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map(|s| s.trim().parse().unwrap())
                    .unwrap_or(0);
                if bytes.len() >= pos + 4 + len {
                    break;
                }
            }
        }
        let body = response.to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let mut client = Client::new();
    let mut config = RequestConfig::default();
    config.url_prefix = format!("http://{addr}/v1");
    // 本番コンストラクタの接続先制限は保持し、テストだけlocalhostへ向ける。
    for option in [
        BitbankOption::HttpUrl(BitbankHttpUrl::None),
        BitbankOption::HttpAuth(true),
        BitbankOption::Key("test-key".into()),
        BitbankOption::Secret("test-secret".into()),
        BitbankOption::RequestConfig(config),
    ] {
        client.update_default_option(option);
    }
    (
        BitbankTradingVenue::new(BitbankPrivateApiClient { client }),
        server,
    )
}

#[tokio::test]
async fn single_order_request_uses_the_correct_method_and_query() {
    let order: Value =
        serde_json::from_str(include_str!("../tests/fixtures/order_snapshot.json")).unwrap();
    let (venue, server) = mock_venue(json!({"success": 1, "data": order}));
    assert_eq!(
        venue.get_order("btc_jpy", OrderId(7)).await.unwrap().status,
        OrderStatus::CanceledPartiallyFilled
    );
    let request = server.join().unwrap();
    let line = request.lines().next().unwrap();
    assert!(line.starts_with("GET /v1/user/spot/order?"));
    assert!(line.contains("pair=btc_jpy"));
    assert!(line.contains("order_id=7"));
    assert!(request.to_lowercase().contains("access-signature:"));
}

#[tokio::test]
async fn history_request_forwards_all_filters_and_converts_the_response() {
    let trade: Value =
        serde_json::from_str(include_str!("../tests/fixtures/own_trade.json")).unwrap();
    let (venue, server) = mock_venue(json!({"success": 1, "data": {"trades": [trade]}}));
    let result = venue
        .trade_history(TradeHistoryQuery {
            pair: Some("btc_jpy".into()),
            order_id: Some(OrderId(7)),
            count: Some(25),
            since: Some(100),
            end: Some(200),
            order: HistoryOrder::Asc,
        })
        .await
        .unwrap();
    assert_eq!(result[0].trade_id, 11);
    let request = server.join().unwrap();
    let line = request.lines().next().unwrap();
    assert!(line.starts_with("GET /v1/user/spot/trade_history?"));
    for param in [
        "pair=btc_jpy",
        "order_id=7",
        "count=25",
        "since=100",
        "end=200",
        "order=asc",
    ] {
        assert!(line.contains(param), "missing query parameter: {param}");
    }
}

#[tokio::test]
async fn missing_orders_are_normalized_and_other_api_errors_remain_errors() {
    let (venue, server) = mock_venue(json!({"success": 0, "data": {"code": 50009}}));
    assert!(matches!(
        venue.get_order("btc_jpy", OrderId(7)).await,
        Err(TradingVenueError::OrderNotFound {
            order_id: OrderId(7),
            ..
        })
    ));
    server.join().unwrap();
    let (venue, server) = mock_venue(json!({"success": 0, "data": {"code": 20001}}));
    assert!(matches!(
        venue.get_order("btc_jpy", OrderId(7)).await,
        Err(TradingVenueError::Bitbank(Some(
            BitbankHandleError::AuthenticationFailed(_)
        )))
    ));
    server.join().unwrap();
}

#[tokio::test]
async fn invalid_history_queries_do_not_access_the_network() {
    let venue = BitbankTradingVenue::new(BitbankPrivateApiClient::new(
        "unused".into(),
        "unused".into(),
        None,
    ));
    assert!(matches!(
        venue
            .trade_history(TradeHistoryQuery {
                count: Some(1001),
                ..Default::default()
            })
            .await,
        Err(TradingVenueError::InvalidQuery(_))
    ));
}
