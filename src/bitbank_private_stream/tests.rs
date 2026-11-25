use super::*;
use crate::bitbank_private::http_tests::serve;
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
};

#[test]
fn decodes_all_documented_methods() {
    fn decode(method: &str, params: Value) -> BitbankPrivateEvent {
        serde_json::from_value(json!({"method": method, "params": params})).unwrap()
    }

    use BitbankPrivateEvent::*;
    for params in [
        json!([{
            "asset": "btc", "amountPrecision": 8, "freeAmount": "1.25",
            "lockedAmount": "0.25", "onhandAmount": "1.50", "withdrawingAmount": "0"
        }]),
        json!([{
            "asset": "btc", "amount_precision": 8, "free_amount": "1.25",
            "locked_amount": "0.25", "onhand_amount": "1.50", "withdrawing_amount": "0"
        }]),
    ] {
        assert!(matches!(
            decode("asset_update", params),
            AssetUpdate(items) if items[0].amount_precision == 8 && items[0].free_amount == "1.25"
        ));
    }

    let mut order = json!([{
        "order_id": 42, "pair": "btc_jpy", "side": "buy", "type": "stop",
        "start_amount": "0.1", "remaining_amount": null, "executed_amount": "0.1",
        "price": null, "trigger_price": "100", "average_price": "99", "status": "FULLY_FILLED",
        "ordered_at": 1, "executed_at": 3, "triggered_at": 2, "expire_at": null,
        "is_just_triggered": true, "user_cancelable": false
    }]);
    assert!(matches!(
        decode("spot_order_new", order.clone()),
        SpotOrderNew(items) if items[0].is_just_triggered == Some(true)
    ));
    order[0]
        .as_object_mut()
        .unwrap()
        .remove("is_just_triggered");
    assert!(matches!(
        decode("spot_order", order),
        SpotOrder(items) if items[0].executed_at.as_ref().and_then(serde_json::Number::as_u64) == Some(3)
            && items[0].is_just_triggered.is_none()
    ));
    assert!(matches!(
        decode("spot_order_invalidation", json!({"order_id": [1, 2, 3]})),
        SpotOrderInvalidation(update) if update.order_id.len() == 3
    ));

    let trade = decode(
        "spot_trade",
        json!([{
            "order_id": 42, "trade_id": 100, "pair": "btc_jpy", "side": "buy", "type": "stop",
            "amount": "0.1", "price": "99", "maker_taker": "taker", "executed_at": 3,
            "fee_amount_base": "0", "fee_amount_quote": "1", "fee_occurred_amount_quote": "1"
        }]),
    );
    assert!(matches!(trade, SpotTrade(items) if items[0].fee_occurred_amount_quote == "1"));

    for id in [json!(42), json!("42")] {
        let dealer_order = decode(
            "dealer_order_new",
            json!([{
                "order_id": id, "asset": "btc", "side": "buy",
                "price": "99", "amount": "1", "ordered_at": 1
            }]),
        );
        assert!(matches!(dealer_order, DealerOrderNew(items) if items[0].order_id == "42"));
    }

    let withdrawal = decode(
        "withdrawal",
        json!([
            {
                "uuid": "fiat-id", "asset": "jpy", "account_uuid": "bank-id", "amount": "900",
                "fee": "100", "bank_name": "bank", "branch_name": "branch",
                "account_type": "ordinary", "account_number": "1234567", "account_owner": "owner",
                "status": "DONE", "requested_at": 1
            },
            {
                "uuid": "crypto-id", "asset": "btc", "account_uuid": "wallet-id", "amount": "1",
                "fee": "0.0006", "network": "bitcoin", "label": "wallet", "address": "sample",
                "txid": null, "status": "SENDING", "requested_at": 2
            }
        ]),
    );
    assert!(matches!(
        withdrawal,
        Withdrawal(items) if items[0].bank_name.as_deref() == Some("bank") && items[1].txid.is_none()
    ));

    for amount in [
        serde_json::from_str("0.12345678901234567890").unwrap(),
        json!("0.12345678901234567890"),
    ] {
        let deposit = decode(
            "deposit",
            json!([{
                "uuid": "crypto-id", "asset": "btc", "amount": amount, "network": "bitcoin",
                "address": "sample", "txid": null, "found_at": 1, "status": "FOUND"
            }]),
        );
        assert!(matches!(
            deposit,
            Deposit(items) if items[0].amount == "0.12345678901234567890" && items[0].confirmed_at.is_none()
        ));
    }

    assert!(matches!(
        decode(
            "margin_position_update",
            json!([{
                "pair": "btc_jpy", "position_side": "long", "average_price": "99", "open_amount": "1",
                "locked_amount": "0", "product": "99", "unrealized_fee_amount": "1",
                "unrealized_interest_amount": "0"
            }]),
        ),
        MarginPositionUpdate(items) if items[0].position_side == "long"
    ));
    assert!(matches!(
        decode("margin_payable_update", json!([{"amount": "100"}])),
        MarginPayableUpdate(items) if items[0].amount == "100"
    ));
    assert!(matches!(
        decode(
            "margin_notice_update",
            json!([{"what": null, "occurred_at": null, "amount": null, "due_date_at": null}]),
        ),
        MarginNoticeUpdate(items) if items[0].what.is_none()
    ));
}

const AUTH_FIRST: &str =
    r#"{"success":1,"data":{"pubnub_channel":"channel-a","pubnub_token":"initial&token"}}"#;
const AUTH_REFRESH_SAME: &str =
    r#"{"success":1,"data":{"pubnub_channel":"channel-a","pubnub_token":"refreshed-token"}}"#;
const AUTH_REFRESH_NEW: &str =
    r#"{"success":1,"data":{"pubnub_channel":"channel-b","pubnub_token":"refreshed-token"}}"#;

#[tokio::test]
async fn resumes_after_disconnect_and_token_expiration() {
    check_resubscribe(false).await;
}

#[tokio::test]
async fn handshakes_again_if_refreshed_channel_changes() {
    check_resubscribe(true).await;
}

async fn check_resubscribe(new_channel: bool) {
    let refresh = if new_channel {
        AUTH_REFRESH_NEW
    } else {
        AUTH_REFRESH_SAME
    };
    let (private_api, auth_server) = serve([
        ("GET /v1/user/subscribe HTTP/1.1", None, AUTH_FIRST),
        ("GET /v1/user/subscribe HTTP/1.1", None, refresh),
    ]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let pubnub_server = std::thread::spawn(move || {
        let empty = r#"{"t":{"t":"12345","r":7},"m":[]}"#;
        let messages = r#"{
            "t":{"t":"67890","r":9},
            "m":[
                {"e":1,"d":"signal"},
                {"d":{"method":"unknown_event","params":[]}},
                {"d":{"method":"spot_order_invalidation","params":{"order_id":[1,2]}}},
                {"d":{"method":"margin_payable_update","params":[{"amount":"100"}]}}
            ]
        }"#;
        let initial = ("channel-a", "initial&token", "0", None);
        let resume = ("channel-a", "initial&token", "12345", Some("7"));
        let refreshed = if new_channel {
            ("channel-b", "refreshed-token", "0", None)
        } else {
            ("channel-a", "refreshed-token", "12345", Some("7"))
        };
        for ((channel, auth, tt, tr), response) in [
            (initial, Some((200, empty))),
            (resume, None),
            (resume, Some((403, ""))),
            (
                refreshed,
                Some((200, if new_channel { empty } else { messages })),
            ),
        ]
        .into_iter()
        .chain(new_channel.then_some((
            ("channel-b", "refreshed-token", "12345", Some("7")),
            Some((200, messages)),
        ))) {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut parts = line.split_whitespace();
            assert_eq!(parts.next(), Some("GET"));
            let url = Url::parse(&format!("http://example.test{}", parts.next().unwrap())).unwrap();
            assert_eq!(
                url.path(),
                format!("/v2/subscribe/{SUBSCRIBE_KEY}/{channel}/0")
            );
            let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
            assert_eq!(query["uuid"], channel);
            assert_eq!(query["auth"], auth);
            assert_eq!(query["tt"], tt);
            assert_eq!(query.get("tr").map(|value| value.as_ref()), tr);
            for line in reader.by_ref().lines() {
                if line.unwrap().is_empty() {
                    break;
                }
            }
            if let Some((status, body)) = response {
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        }
    });

    tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream = BitbankPrivateStream::connect_to(&private_api, endpoint)
            .await
            .unwrap();
        assert!(matches!(
            stream.next_event().await,
            Err(BitbankPrivateStreamError::InvalidEvent)
        ));
        assert!(matches!(
            stream.next_event().await.unwrap(),
            BitbankPrivateEvent::SpotOrderInvalidation(update) if update.order_id.len() == 2
        ));
        assert!(matches!(
            stream.next_event().await.unwrap(),
            BitbankPrivateEvent::MarginPayableUpdate(items) if items[0].amount == "100"
        ));
        assert_eq!(stream.cursor.as_ref().unwrap().timetoken, "67890");
        stream.close();
    })
    .await
    .unwrap();
    auth_server.join().unwrap();
    pubnub_server.join().unwrap();
}

#[cfg(feature = "live-private-api")]
#[tokio::test]
async fn live_private_stream_subscribe() {
    let private_api = BitbankPrivateApiClient::new(
        std::env::var("BITBANK_API_KEY").expect("BITBANK_API_KEY is required"),
        std::env::var("BITBANK_API_SECRET").expect("BITBANK_API_SECRET is required"),
        None,
    );
    let mut stream = BitbankPrivateStream::connect(&private_api).await.unwrap();
    println!("PubNub initial handshake succeeded");
    stream.subscribe().await.unwrap();
    println!("PubNub follow-up poll succeeded");

    // 口座の操作は行わないため、通知が届いた場合だけデコードする。
    let mut decoded = 0;
    let observation = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let event = stream.next_event().await.unwrap();
            decoded += 1;
            match event {
                BitbankPrivateEvent::AssetUpdate(_) => println!("Decoded asset update"),
                BitbankPrivateEvent::SpotOrderNew(_) => println!("Decoded new spot order"),
                BitbankPrivateEvent::SpotOrder(_) => println!("Decoded spot order update"),
                _ => println!("Decoded account notification"),
            }
        }
    });
    let _ = observation.await;
    println!("Decoded account notifications: {decoded}");
    stream.close();
}
