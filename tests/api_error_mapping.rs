//! 依存先のAPIエラー分類とbbutilのレスポンス処理を通した回帰テスト。
use bitbankutil_rs::{bitbank_structs::BitbankApiResponse, response_handler::handle_response};
use crypto_botters::{
    bitbank::{BitbankHandleError, BitbankOption, BitbankOptions},
    http::{Bytes, HeaderMap, RequestError, RequestHandler, StatusCode},
    traits::HttpOption,
};
use serde_json::{Value, json};

#[test]
fn official_error_codes_reach_callers_with_original_response() {
    let handler = <BitbankOption as HttpOption<'_, BitbankApiResponse, ()>>::request_handler(
        BitbankOptions::default(),
    );
    macro_rules! check {
        ($code:literal, $variant:ident) => {
            for status in [StatusCode::OK, StatusCode::BAD_REQUEST] {
                let body = json!({"success": 0, "data": {"code": $code, "detail": "kept"}});
                let response = <_ as RequestHandler<()>>::handle_response(
                    &handler, status, HeaderMap::new(), Bytes::from(body.to_string()),
                ).map_err(RequestError::ResponseHandleError);
                match handle_response::<Value>("test", response) {
                    Err(Some(BitbankHandleError::$variant(value))) => assert_eq!(value, body),
                    other => panic!("code {}: {:?}", $code, other),
                }
            }
        };
    }
    check!(20019, PleaseLogin);
    check!(40164, InvalidPositionSide);
    check!(40167, MarginTradingNotSupportedForPair);
    check!(40202, StopEntryOrderNotAllowed);
    check!(50058, MarginTradingReviewIncomplete);
    check!(50059, MarginEntryOrderTemporarilyRestricted);
    check!(50060, MarginEntryOrderTemporarilyRestricted);
    check!(50061, MarginEntryAmountExceeded);
    check!(50062, PositionQuantityExceeded);
    check!(50073, JpyWithdrawalUnavailable);
    check!(50074, CryptoWithdrawalUnavailable);
    check!(50075, SpotBuyOrderUnavailable);
    check!(50076, SpotSellOrderUnavailable);
    check!(50081, MarginEntrySellOrderUnavailable);
    check!(50082, MarginSettlementSellOrderUnavailable);
    check!(50083, MarginEntryBuyOrderUnavailable);
    check!(50084, MarginSettlementBuyOrderUnavailable);
    check!(50086, WithdrawalUnavailableDueToSettlementLoss);
    check!(50088, WithdrawalUnavailableDueToReceivables);
    check!(60018, ImmediatelyTriggeredPriceNotAllowed);
    check!(60019, TakeProfitOrStopLossSideMustSettlePosition);
    check!(60020, SingleWithdrawalAmountExceeded);
    check!(70024, LongEntryOrderSuspended);
    check!(70025, ShortEntryOrderSuspended);
    check!(70026, OrderAcceptanceTemporarilyRestricted);
}

#[test]
fn unknown_error_code_remains_api_error() {
    let handler = <BitbankOption as HttpOption<'_, BitbankApiResponse, ()>>::request_handler(
        BitbankOptions::default(),
    );
    let body = json!({"success": 0, "data": {"code": 99999}});
    let response = <_ as RequestHandler<()>>::handle_response(
        &handler,
        StatusCode::OK,
        HeaderMap::new(),
        Bytes::from(body.to_string()),
    )
    .map_err(RequestError::ResponseHandleError);
    match handle_response::<Value>("test", response) {
        Err(Some(BitbankHandleError::ApiError(value))) => assert_eq!(value, body),
        other => panic!("unexpected result: {:?}", other),
    }
}
