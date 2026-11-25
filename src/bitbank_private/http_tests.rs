use super::*;
use crypto_botters::http::RequestConfig;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread::JoinHandle,
    time::Duration,
};

pub(crate) fn serve<const N: usize>(
    exchanges: [(&'static str, Option<Value>, &'static str); N],
) -> (BitbankPrivateApiClient, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url_prefix = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for (request_line, expected_body, response) in exchanges {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), request_line);

            let mut content_length = 0;
            let mut authenticated = false;
            for line in reader.by_ref().lines() {
                let line = line.unwrap();
                if line.is_empty() {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap();
                }
                if name.eq_ignore_ascii_case("access-signature") {
                    authenticated = !value.trim().is_empty();
                }
            }
            assert!(authenticated);
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            if let Some(expected) = expected_body {
                assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected);
            } else {
                assert!(body.is_empty());
            }
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .unwrap();
        }
    });

    let mut client = BitbankPrivateApiClient::new("test-key".into(), "test-secret".into(), None);
    client
        .options
        .update(BitbankOption::HttpUrl(BitbankHttpUrl::None));
    let mut config = RequestConfig::default();
    config.url_prefix = url_prefix;
    config.timeout = Duration::from_secs(5);
    client.options.update(BitbankOption::RequestConfig(config));
    (client, server)
}

#[tokio::test]
async fn deposit_history_filters_and_decodes_crypto_and_jpy() {
    let (client, server) = serve([
        (
            "GET /v1/user/deposit_history?asset=jpy&count=100&end=1764000000123&since=1763000000123 HTTP/1.1",
            None,
            r#"{"success":1,"data":{"deposits":[{"uuid":"jpy-deposit","asset":"jpy",
                "amount":1234567890.123456789012345678,"txid":null,"status":"DONE",
                "category":"NORMAL","found_at":1763500000123,"confirmed_at":1763500000456}]}}"#,
        ),
        (
            "GET /v1/user/deposit_history HTTP/1.1",
            None,
            r#"{"success":1,"data":{"deposits":[{"uuid":"btc-deposit","asset":"btc",
                "address":"deposit-address","network":"bitcoin","amount":"0.01234567",
                "txid":"btc-tx","status":"FOUND","category":"NORMAL","found_at":1763500000123}]}}"#,
        ),
    ]);

    let jpy = client
        .get_deposit_history(
            Some("jpy"),
            Some(100),
            Some(1763000000123),
            Some(1764000000123),
        )
        .await
        .unwrap();
    let [jpy] = jpy.deposits.as_slice() else {
        panic!("expected one JPY deposit");
    };
    assert_eq!(jpy.asset, "jpy");
    assert_eq!(jpy.amount, "1234567890.123456789012345678");
    assert_eq!(jpy.address, None);
    assert_eq!(jpy.network, None);
    assert_eq!(jpy.txid, None);
    assert_eq!(jpy.confirmed_at, Some(1763500000456));

    let btc = client
        .get_deposit_history(None, None, None, None)
        .await
        .unwrap();
    let [btc] = btc.deposits.as_slice() else {
        panic!("expected one BTC deposit");
    };
    assert_eq!(btc.uuid, "btc-deposit");
    assert_eq!(btc.network.as_deref(), Some("bitcoin"));
    assert_eq!(btc.address.as_deref(), Some("deposit-address"));
    assert_eq!(btc.txid.as_deref(), Some("btc-tx"));
    assert_eq!(btc.amount, "0.01234567");
    assert_eq!(btc.found_at, 1763500000123);
    assert_eq!(btc.confirmed_at, None);
    server.join().unwrap();
}

#[tokio::test]
async fn unconfirmed_deposits_and_originators_decode() {
    let (client, server) = serve([
        (
            "GET /v1/user/unconfirmed_deposits HTTP/1.1",
            None,
            r#"{"success":1,"data":{"deposits":[{"uuid":"pending","asset":"btc",
                "amount":"0.10","network":"bitcoin","txid":"btc-tx","created_at":1763500000123}]}}"#,
        ),
        (
            "GET /v1/user/deposit_originators HTTP/1.1",
            None,
            r#"{"success":1,"data":{"originators":[{"uuid":"company","label":"company wallet",
                "deposit_type":"WALLET","deposit_purpose":"payment","originator_status":"CONFIRMED",
                "originator_type":"COMPANY","originator_last_name":null,"originator_first_name":null,
                "originator_country":"JP","originator_prefecture":"Tokyo","originator_city":"Chiyoda",
                "originator_address":"test address","originator_building":null,
                "originator_company_name":"Example","originator_company_type":"corporation",
                "originator_company_type_position":"suffix","originator_substantial_controllers":[
                    {"uuid":"controller","name":"Example Person","country":"JP","prefecture":null}
                ]}]}}"#,
        ),
    ]);

    let pending = client.get_unconfirmed_deposits().await.unwrap();
    let [pending] = pending.deposits.as_slice() else {
        panic!("expected one unconfirmed deposit");
    };
    assert_eq!(pending.uuid, "pending");
    assert_eq!(pending.amount, "0.10");
    assert_eq!(pending.created_at, 1763500000123);

    let originators = client.get_deposit_originators().await.unwrap();
    let [originator] = originators.originators.as_slice() else {
        panic!("expected one originator");
    };
    assert_eq!(originator.uuid, "company");
    assert_eq!(originator.originator_last_name, None);
    let [controller] = originator
        .originator_substantial_controllers
        .as_deref()
        .unwrap()
    else {
        panic!("expected one substantial controller");
    };
    assert_eq!(controller.uuid, "controller");
    assert_eq!(controller.prefecture, None);
    server.join().unwrap();
}

#[tokio::test]
async fn confirm_deposits_sends_expected_bodies_and_propagates_api_error() {
    let all_body = json!({ "originator_uuid": "wallet" });
    let (client, server) = serve([
        (
            "POST /v1/user/confirm_deposits HTTP/1.1",
            Some(json!({"deposits": [
                {"uuid":"deposit-1","originator_uuid":"wallet"},
                {"uuid":"deposit-2","originator_uuid":"company"}
            ]})),
            r#"{"success":1,"data":{}}"#,
        ),
        (
            "POST /v1/user/confirm_deposits_all HTTP/1.1",
            Some(all_body.clone()),
            r#"{"success":1,"data":{}}"#,
        ),
        (
            "POST /v1/user/confirm_deposits_all HTTP/1.1",
            Some(all_body),
            r#"{"success":0,"data":{"code":20019}}"#,
        ),
    ]);

    client
        .post_confirm_deposits(&[
            BitbankDepositConfirmation {
                uuid: "deposit-1",
                originator_uuid: "wallet",
            },
            BitbankDepositConfirmation {
                uuid: "deposit-2",
                originator_uuid: "company",
            },
        ])
        .await
        .unwrap();
    client.post_confirm_deposits_all("wallet").await.unwrap();
    assert!(matches!(
        client.post_confirm_deposits_all("wallet").await,
        Err(Some(BitbankHandleError::PleaseLogin(_)))
    ));
    server.join().unwrap();
}

#[tokio::test]
async fn orders_info_sends_ids_and_preserves_partial_and_empty_results() {
    let request_line = "POST /v1/user/spot/orders_info HTTP/1.1";
    let (client, server) = serve([
        (
            request_line,
            Some(json!({"pair": "btc_jpy", "order_ids": [101, 102, 103]})),
            r#"{"success":1,"data":{"orders":[
                {"order_id":103,"pair":"btc_jpy","side":"sell","type":"limit",
                    "executed_amount":"0","user_cancelable":false,"average_price":"0",
                    "ordered_at":1000,"canceled_at":2000,"status":"CANCELED_UNFILLED"},
                {"order_id":101,"pair":"btc_jpy","side":"buy","type":"stop",
                    "executed_amount":"1","user_cancelable":false,"average_price":"100",
                    "ordered_at":1000,"triggered_at":1500,"status":"FULLY_FILLED"}
            ]}}"#,
        ),
        (
            request_line,
            Some(json!({"pair": "btc_jpy", "order_ids": [102]})),
            r#"{"success":1,"data":{"orders":[]}}"#,
        ),
    ]);

    let result = client
        .post_orders_info("btc_jpy", &[101, 102, 103])
        .await
        .unwrap();
    let [canceled, triggered] = result.orders.as_slice() else {
        panic!("expected two orders");
    };
    assert_eq!(canceled.order_id.as_u64(), Some(103));
    assert_eq!(
        canceled.canceled_at.as_ref().and_then(|ts| ts.as_u64()),
        Some(2000)
    );
    assert_eq!(triggered.order_id.as_u64(), Some(101));
    assert_eq!(
        triggered.triggered_at.as_ref().and_then(|ts| ts.as_u64()),
        Some(1500)
    );

    let result = client.post_orders_info("btc_jpy", &[102]).await.unwrap();
    assert!(result.orders.is_empty());
    server.join().unwrap();
}

#[tokio::test]
async fn margin_queries_decode_status_positions_and_nullable_notices() {
    let (client, server) = serve([
        (
            "GET /v1/user/margin/status HTTP/1.1",
            None,
            r#"{"success":1,"data":{
                "status":"NORMAL",
                "total_margin_balance_percentage":null,
                "total_margin_balance":"12.3400",
                "margin_position_profit_loss":"0.0000",
                "unrealized_cost":"0.0000",
                "total_margin_position_product":"0.0000",
                "open_margin_position_product":"0.0000",
                "open_margin_order_product":"0.0000",
                "total_position_maintenance_margin":"0.0000",
                "total_long_position_maintenance_margin":"0.0000",
                "total_short_position_maintenance_margin":"0.0000",
                "total_open_order_maintenance_margin":"0.0000",
                "total_long_open_order_maintenance_margin":"0.0000",
                "total_short_open_order_maintenance_margin":"0.0000",
                "margin_call_percentage":null,
                "losscut_percentage":null,
                "buy_credit":"50000",
                "sell_credit":"10000",
                "available_balances":[{"pair":"btc_jpy","long":"24.6800","short":"10.0000"}]
            }}"#,
        ),
        (
            "GET /v1/user/margin/positions HTTP/1.1",
            None,
            r#"{"success":1,"data":{
                "notice":{"what":null,"occurred_at":null,"amount":null,"due_date_at":null},
                "payables":{"amount":"0"},
                "positions":[],
                "losscut_threshold":{"individual":"50","company":"100"}
            }}"#,
        ),
        (
            "GET /v1/user/margin/positions HTTP/1.1",
            None,
            r#"{"success":1,"data":{
                "notice":{"what":"margin_call","occurred_at":1795507200000,
                    "amount":"1000.12","due_date_at":1795593600000},
                "payables":{"amount":"10.9900"},
                "positions":[{"pair":"btc_jpy","position_side":"long","open_amount":"0.0200",
                    "locked_amount":"0.0100","product":"200000","average_price":"10000000",
                    "unrealized_fee_amount":"0.0010","unrealized_interest_amount":"0.0020"}],
                "losscut_threshold":{"individual":"50","company":"100"}
            }}"#,
        ),
    ]);

    let status = client.get_margin_status().await.unwrap();
    assert_eq!(status.status, "NORMAL");
    assert_eq!(status.total_margin_balance, "12.3400");
    assert_eq!(status.total_margin_balance_percentage, None);
    assert_eq!(status.margin_call_percentage, None);
    assert_eq!(status.losscut_percentage, None);
    assert_eq!(status.available_balances[0].pair, "btc_jpy");
    assert_eq!(status.available_balances[0].long, "24.6800");
    assert_eq!(status.available_balances[0].short, "10.0000");

    let empty = client.get_margin_positions().await.unwrap();
    assert!(empty.positions.is_empty());
    assert_eq!(empty.notice.what, None);
    assert_eq!(empty.notice.occurred_at, None);
    assert_eq!(empty.notice.amount, None);
    assert_eq!(empty.notice.due_date_at, None);

    let populated = client.get_margin_positions().await.unwrap();
    assert_eq!(populated.positions[0].locked_amount, "0.0100");
    assert_eq!(populated.notice.what.as_deref(), Some("margin_call"));
    assert_eq!(populated.notice.amount.as_deref(), Some("1000.12"));
    assert_eq!(
        populated.notice.occurred_at.unwrap().as_u64(),
        Some(1795507200000)
    );
    assert_eq!(
        populated.notice.due_date_at.unwrap().as_u64(),
        Some(1795593600000)
    );
    assert_eq!(populated.payables.amount, "10.9900");
    assert_eq!(populated.losscut_threshold.individual, "50");
    assert_eq!(populated.losscut_threshold.company, "100");
    server.join().unwrap();
}
