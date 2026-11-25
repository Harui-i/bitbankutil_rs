use super::*;
use crypto_botters::http::RequestConfig;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread::JoinHandle,
    time::Duration,
};

fn serve<const N: usize>(
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
