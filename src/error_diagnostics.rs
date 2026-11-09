//! Safe, payload-free error classification shared by all logging layers.
use crate::{
    book_units::NumericError, order_domain::ParseOrderError, trading_venue::TradingVenueError,
};
use crypto_botters::bitbank::BitbankHandleError as B;

pub(crate) fn numeric(error: &NumericError) -> &'static str {
    match error {
        NumericError::Invalid => "invalid_number",
        NumericError::Precision => "numeric_precision",
        NumericError::Overflow => "numeric_overflow",
        NumericError::DivisionByZero => "division_by_zero",
    }
}
pub(crate) fn parse(error: &ParseOrderError) -> &'static str {
    match error {
        ParseOrderError::UnknownSide(_) => "unknown_side",
        ParseOrderError::UnknownType(_) => "unknown_order_type",
        ParseOrderError::InvalidOrderId => "invalid_order_id",
        ParseOrderError::MissingRemainingAmount => "missing_remaining_amount",
        ParseOrderError::InvalidNumeric { error, .. } => numeric(error),
    }
}

pub(crate) fn api(error: &Option<B>) -> (&'static str, Option<u64>, bool) {
    let Some(error) = error else {
        return ("unavailable_error_detail", None, false);
    };
    api_ref(error)
}
pub(crate) fn api_ref(error: &B) -> (&'static str, Option<u64>, bool) {
    let (reason, payload): (&str, Option<&serde_json::Value>) = match error {
        B::ApiError(value) => ("api_error", Some(value)),
        B::RequestLimitExceeded(value) => ("request_limit_exceeded", Some(value)),
        B::ParseError => ("parse_error", None),
        B::UrlNotFound(value) => ("url_not_found", Some(value)),
        B::SystemError(value) => ("system_error", Some(value)),
        B::MalformedRequest(value) => ("malformed_request", Some(value)),
        B::TimeoutWaitingForResponse(value) => ("timeout_waiting_for_response", Some(value)),
        B::SystemMaintenance(value) => ("system_maintenance", Some(value)),
        B::ServerIsBusy(value) => ("server_is_busy", Some(value)),
        B::RequestTooFrequent(value) => ("request_too_frequent", Some(value)),
        B::AuthenticationFailed(value) => ("authentication_failed", Some(value)),
        B::InvalidAccessKey(value) => ("invalid_access_key", Some(value)),
        B::AccessKeyNotFound(value) => ("access_key_not_found", Some(value)),
        B::AccessNonceNotFound(value) => ("access_nonce_not_found", Some(value)),
        B::InvalidAccessSignature(value) => ("invalid_access_signature", Some(value)),
        B::MfaFailed(value) => ("mfa_failed", Some(value)),
        B::SmsVerificationFailed(value) => ("sms_verification_failed", Some(value)),
        B::PleaseLogin(value) => ("please_login", Some(value)),
        B::MissingOtpCode(value) => ("missing_otp_code", Some(value)),
        B::MissingSmsCode(value) => ("missing_sms_code", Some(value)),
        B::MissingOtpAndSmsCode(value) => ("missing_otp_and_sms_code", Some(value)),
        B::MfaTemporarilyLocked(value) => ("mfa_temporarily_locked", Some(value)),
        B::MissingAccessRequestTime(value) => ("missing_access_request_time", Some(value)),
        B::InvalidAccessRequestTime(value) => ("invalid_access_request_time", Some(value)),
        B::NoRequestSentWithinAccessTimeWindow(value) => {
            ("no_request_sent_within_access_time_window", Some(value))
        }
        B::AccessRequestTimeAndNonceNotFound(value) => {
            ("access_request_time_and_nonce_not_found", Some(value))
        }
        B::InvalidAccessTimeWindow(value) => ("invalid_access_time_window", Some(value)),
        B::InvalidAccessNonce(value) => ("invalid_access_nonce", Some(value)),
        B::MissingOrderQuantity(value) => ("missing_order_quantity", Some(value)),
        B::MissingOrderId(value) => ("missing_order_id", Some(value)),
        B::MissingOrderIds(value) => ("missing_order_ids", Some(value)),
        B::MissingAsset(value) => ("missing_asset", Some(value)),
        B::MissingOrderPrice(value) => ("missing_order_price", Some(value)),
        B::MissingSide(value) => ("missing_side", Some(value)),
        B::MissingOrderType(value) => ("missing_order_type", Some(value)),
        B::MissingUuid(value) => ("missing_uuid", Some(value)),
        B::MissingWithdrawAmount(value) => ("missing_withdraw_amount", Some(value)),
        B::MissingTriggerPrice(value) => ("missing_trigger_price", Some(value)),
        B::MissingWithdrawalType(value) => ("missing_withdrawal_type", Some(value)),
        B::MissingWithdrawalName(value) => ("missing_withdrawal_name", Some(value)),
        B::MissingVasp(value) => ("missing_vasp", Some(value)),
        B::MissingBeneficiaryType(value) => ("missing_beneficiary_type", Some(value)),
        B::MissingBeneficiaryLastName(value) => ("missing_beneficiary_last_name", Some(value)),
        B::MissingBeneficiaryFirstName(value) => ("missing_beneficiary_first_name", Some(value)),
        B::MissingBeneficiaryLastKana(value) => ("missing_beneficiary_last_kana", Some(value)),
        B::MissingBeneficiaryFirstKana(value) => ("missing_beneficiary_first_kana", Some(value)),
        B::MissingBeneficiaryCompanyName(value) => {
            ("missing_beneficiary_company_name", Some(value))
        }
        B::MissingBeneficiaryCompanyKana(value) => {
            ("missing_beneficiary_company_kana", Some(value))
        }
        B::MissingBeneficiaryCompanyType(value) => {
            ("missing_beneficiary_company_type", Some(value))
        }
        B::MissingBeneficiaryCompanyTypePosition(value) => {
            ("missing_beneficiary_company_type_position", Some(value))
        }
        B::MissingUploadedDocuments(value) => ("missing_uploaded_documents", Some(value)),
        B::MissingWithdrawalPurpose(value) => ("missing_withdrawal_purpose", Some(value)),
        B::MissingBeneficiaryCountry(value) => ("missing_beneficiary_country", Some(value)),
        B::MissingBeneficiaryZipCode(value) => ("missing_beneficiary_zip_code", Some(value)),
        B::MissingBeneficiaryPrefecture(value) => ("missing_beneficiary_prefecture", Some(value)),
        B::MissingBeneficiaryCity(value) => ("missing_beneficiary_city", Some(value)),
        B::MissingBeneficiaryAddress(value) => ("missing_beneficiary_address", Some(value)),
        B::MissingBeneficiaryBuilding(value) => ("missing_beneficiary_building", Some(value)),
        B::MissingExtractionRequestCategory(value) => {
            ("missing_extraction_request_category", Some(value))
        }
        B::InvalidOrderQuantity(value) => ("invalid_order_quantity", Some(value)),
        B::InvalidCount(value) => ("invalid_count", Some(value)),
        B::InvalidEndParam(value) => ("invalid_end_param", Some(value)),
        B::InvalidEndId(value) => ("invalid_end_id", Some(value)),
        B::InvalidFromId(value) => ("invalid_from_id", Some(value)),
        B::InvalidOrderId(value) => ("invalid_order_id", Some(value)),
        B::InvalidOrderIds(value) => ("invalid_order_ids", Some(value)),
        B::TooManyOrdersSpecified(value) => ("too_many_orders_specified", Some(value)),
        B::InvalidAsset(value) => ("invalid_asset", Some(value)),
        B::InvalidOrderPrice(value) => ("invalid_order_price", Some(value)),
        B::InvalidOrderSide(value) => ("invalid_order_side", Some(value)),
        B::InvalidTradingStartTime(value) => ("invalid_trading_start_time", Some(value)),
        B::InvalidOrderType(value) => ("invalid_order_type", Some(value)),
        B::InvalidUuid(value) => ("invalid_uuid", Some(value)),
        B::InvalidWithdrawAmount(value) => ("invalid_withdraw_amount", Some(value)),
        B::InvalidTriggerPrice(value) => ("invalid_trigger_price", Some(value)),
        B::InvalidPostOnly(value) => ("invalid_post_only", Some(value)),
        B::PostOnlyCannotBeSpecifiedWithSuchOrderType(value) => (
            "post_only_cannot_be_specified_with_such_order_type",
            Some(value),
        ),
        B::InvalidWithdrawalType(value) => ("invalid_withdrawal_type", Some(value)),
        B::InvalidWithdrawalName(value) => ("invalid_withdrawal_name", Some(value)),
        B::InvalidVasp(value) => ("invalid_vasp", Some(value)),
        B::InvalidBeneficiaryType(value) => ("invalid_beneficiary_type", Some(value)),
        B::InvalidBeneficiaryLastName(value) => ("invalid_beneficiary_last_name", Some(value)),
        B::InvalidBeneficiaryFirstName(value) => ("invalid_beneficiary_first_name", Some(value)),
        B::InvalidBeneficiaryLastKana(value) => ("invalid_beneficiary_last_kana", Some(value)),
        B::InvalidBeneficiaryFirstKana(value) => ("invalid_beneficiary_first_kana", Some(value)),
        B::InvalidBeneficiaryCompanyName(value) => {
            ("invalid_beneficiary_company_name", Some(value))
        }
        B::InvalidBeneficiaryCompanyKana(value) => {
            ("invalid_beneficiary_company_kana", Some(value))
        }
        B::InvalidBeneficiaryCompanyType(value) => {
            ("invalid_beneficiary_company_type", Some(value))
        }
        B::InvalidBeneficiaryCompanyTypePosition(value) => {
            ("invalid_beneficiary_company_type_position", Some(value))
        }
        B::InvalidOriginatorLabel(value) => ("invalid_originator_label", Some(value)),
        B::InvalidOriginatorLastName(value) => ("invalid_originator_last_name", Some(value)),
        B::InvalidOriginatorFirstName(value) => ("invalid_originator_first_name", Some(value)),
        B::InvalidOriginatorCompanyName(value) => ("invalid_originator_company_name", Some(value)),
        B::InvalidOriginatorPrefecture(value) => ("invalid_originator_prefecture", Some(value)),
        B::InvalidOriginatorCity(value) => ("invalid_originator_city", Some(value)),
        B::InvalidOriginatorAddress(value) => ("invalid_originator_address", Some(value)),
        B::InvalidOriginatorBuilding(value) => ("invalid_originator_building", Some(value)),
        B::InvalidOriginatorSubstantialControllerName(value) => (
            "invalid_originator_substantial_controller_name",
            Some(value),
        ),
        B::InvalidBeneficiarySubstantialControllerName(value) => (
            "invalid_beneficiary_substantial_controller_name",
            Some(value),
        ),
        B::InvalidPositionSide(value) => ("invalid_position_side", Some(value)),
        B::MarginTradingNotSupportedForPair(value) => {
            ("margin_trading_not_supported_for_pair", Some(value))
        }
        B::StopEntryOrderNotAllowed(value) => ("stop_entry_order_not_allowed", Some(value)),
        B::AccountIsRestricted(value) => ("account_is_restricted", Some(value)),
        B::AccountIsProvisional(value) => ("account_is_provisional", Some(value)),
        B::AccountIsBlocked(value) => ("account_is_blocked", Some(value)),
        B::IdentityVerificationIsNotFinished(value) => {
            ("identity_verification_is_not_finished", Some(value))
        }
        B::OrderNotFound(value) => ("order_not_found", Some(value)),
        B::OrderCannotBeCanceled(value) => ("order_cannot_be_canceled", Some(value)),
        B::ApiNotFound(value) => ("api_not_found", Some(value)),
        B::OrderHasAlreadyBeenCanceled(value) => ("order_has_already_been_canceled", Some(value)),
        B::OrderHasAlreadyBeenExecuted(value) => ("order_has_already_been_executed", Some(value)),
        B::WithdrawalsToThisAddressRequireAdditionalEntries(value) => (
            "withdrawals_to_this_address_require_additional_entries",
            Some(value),
        ),
        B::VaspNotFound(value) => ("vasp_not_found", Some(value)),
        B::CompanyInformationIsNotRegistered(value) => {
            ("company_information_is_not_registered", Some(value))
        }
        B::WithdrawalsTemporarilyRestricted(value) => {
            ("withdrawals_temporarily_restricted", Some(value))
        }
        B::CannotWithdrawToChosenVaspService(value) => {
            ("cannot_withdraw_to_chosen_vasp_service", Some(value))
        }
        B::OriginatorAlreadyRegistered(value) => ("originator_already_registered", Some(value)),
        B::OriginatorNotFound(value) => ("originator_not_found", Some(value)),
        B::DepositNotFound(value) => ("deposit_not_found", Some(value)),
        B::CannotEditBeneficiaryUnderReview(value) => {
            ("cannot_edit_beneficiary_under_review", Some(value))
        }
        B::CannotEditDisabledBeneficiary(value) => {
            ("cannot_edit_disabled_beneficiary", Some(value))
        }
        B::CannotWithdrawToBeneficiaryUnderReview(value) => {
            ("cannot_withdraw_to_beneficiary_under_review", Some(value))
        }
        B::BeneficiaryRequiresAdditionalEntries(value) => {
            ("beneficiary_requires_additional_entries", Some(value))
        }
        B::CannotWithdrawToChosenBeneficiary(value) => {
            ("cannot_withdraw_to_chosen_beneficiary", Some(value))
        }
        B::CannotConfirmDepositWithOriginatorUnderReview(value) => (
            "cannot_confirm_deposit_with_originator_under_review",
            Some(value),
        ),
        B::OriginatorRequiresAdditionalEntries(value) => {
            ("originator_requires_additional_entries", Some(value))
        }
        B::CannotEditOriginatorUnderReview(value) => {
            ("cannot_edit_originator_under_review", Some(value))
        }
        B::MarginTradingReviewIncomplete(value) => {
            ("margin_trading_review_incomplete", Some(value))
        }
        B::MarginEntryOrderTemporarilyRestricted(value) => {
            ("margin_entry_order_temporarily_restricted", Some(value))
        }
        B::MarginEntryAmountExceeded(value) => ("margin_entry_amount_exceeded", Some(value)),
        B::PositionQuantityExceeded(value) => ("position_quantity_exceeded", Some(value)),
        B::JpyWithdrawalUnavailable(value) => ("jpy_withdrawal_unavailable", Some(value)),
        B::CryptoWithdrawalUnavailable(value) => ("crypto_withdrawal_unavailable", Some(value)),
        B::SpotBuyOrderUnavailable(value) => ("spot_buy_order_unavailable", Some(value)),
        B::SpotSellOrderUnavailable(value) => ("spot_sell_order_unavailable", Some(value)),
        B::MarginEntrySellOrderUnavailable(value) => {
            ("margin_entry_sell_order_unavailable", Some(value))
        }
        B::MarginSettlementSellOrderUnavailable(value) => {
            ("margin_settlement_sell_order_unavailable", Some(value))
        }
        B::MarginEntryBuyOrderUnavailable(value) => {
            ("margin_entry_buy_order_unavailable", Some(value))
        }
        B::MarginSettlementBuyOrderUnavailable(value) => {
            ("margin_settlement_buy_order_unavailable", Some(value))
        }
        B::WithdrawalUnavailableDueToSettlementLoss(value) => {
            ("withdrawal_unavailable_due_to_settlement_loss", Some(value))
        }
        B::WithdrawalUnavailableDueToReceivables(value) => {
            ("withdrawal_unavailable_due_to_receivables", Some(value))
        }
        B::InsufficientAmount(value) => ("insufficient_amount", Some(value)),
        B::MarketBuyOrderQuantityHasExceededTheUpperLimit(value) => (
            "market_buy_order_quantity_has_exceeded_the_upper_limit",
            Some(value),
        ),
        B::OrderQuantityHasExceededTheLimit(value) => {
            ("order_quantity_has_exceeded_the_limit", Some(value))
        }
        B::OrderQuantityHasExceededTheLowerThreshold(value) => (
            "order_quantity_has_exceeded_the_lower_threshold",
            Some(value),
        ),
        B::OrderPriceHasExceededTheUpperLimit(value) => {
            ("order_price_has_exceeded_the_upper_limit", Some(value))
        }
        B::OrderPriceHasExceededTheLowerLimit(value) => {
            ("order_price_has_exceeded_the_lower_limit", Some(value))
        }
        B::TooManySimultaneousOrders(value) => ("too_many_simultaneous_orders", Some(value)),
        B::TriggerPriceHasExceededTheUpperLimit(value) => {
            ("trigger_price_has_exceeded_the_upper_limit", Some(value))
        }
        B::WithdrawalAmountHasExceededTheUpperLimit(value) => (
            "withdrawal_amount_has_exceeded_the_upper_limit",
            Some(value),
        ),
        B::ImmediatelyTriggeredPriceNotAllowed(value) => {
            ("immediately_triggered_price_not_allowed", Some(value))
        }
        B::TakeProfitOrStopLossSideMustSettlePosition(value) => (
            "take_profit_or_stop_loss_side_must_settle_position",
            Some(value),
        ),
        B::SingleWithdrawalAmountExceeded(value) => {
            ("single_withdrawal_amount_exceeded", Some(value))
        }
        B::SystemErrorStopUpdateRequest(value) => ("system_error_stop_update_request", Some(value)),
        B::OrderIsRestrictedDuringSuspensionOfTransactions(value) => (
            "order_is_restricted_during_suspension_of_transactions",
            Some(value),
        ),
        B::BuyOrderHasTemporarilyBeenRestricted(value) => {
            ("buy_order_has_temporarily_been_restricted", Some(value))
        }
        B::SellOrderHasTemporarilyBeenRestricted(value) => {
            ("sell_order_has_temporarily_been_restricted", Some(value))
        }
        B::MarketOrderHasTemporarilyBeenRestricted(value) => {
            ("market_order_has_temporarily_been_restricted", Some(value))
        }
        B::MinimumOrderQuantityIsIncreasedTemporarily(value) => (
            "minimum_order_quantity_is_increased_temporarily",
            Some(value),
        ),
        B::SystemIsBusyStopUpdateRequest(value) => {
            ("system_is_busy_stop_update_request", Some(value))
        }
        B::OrderAndCancelHasTemporarilyBeenRestricted(value) => (
            "order_and_cancel_has_temporarily_been_restricted",
            Some(value),
        ),
        B::WithdrawAndCancelRequestHasTemporarilyBeenRestricted(value) => (
            "withdraw_and_cancel_request_has_temporarily_been_restricted",
            Some(value),
        ),
        B::LendingAndCancelRequestHasTemporarilyBeenRestricted(value) => (
            "lending_and_cancel_request_has_temporarily_been_restricted",
            Some(value),
        ),
        B::LendingAndCancelRequestHasRestricted(value) => {
            ("lending_and_cancel_request_has_restricted", Some(value))
        }
        B::OrdersOnPairHaveBeenSuspended(value) => {
            ("orders_on_pair_have_been_suspended", Some(value))
        }
        B::OrderAndCancelOnPairHaveBeenSuspended(value) => {
            ("order_and_cancel_on_pair_have_been_suspended", Some(value))
        }
        B::OrderCancelRequestIsInProgress(value) => {
            ("order_cancel_request_is_in_progress", Some(value))
        }
        B::LimitOrderPriceIsOverTheThreshold(value) => {
            ("limit_order_price_is_over_the_threshold", Some(value))
        }
        B::StopLimitOrderHasTemporarilyBeenRestricted(value) => (
            "stop_limit_order_has_temporarily_been_restricted",
            Some(value),
        ),
        B::StopOrderHasTemporarilyBeenRestricted(value) => {
            ("stop_order_has_temporarily_been_restricted", Some(value))
        }
        B::LongEntryOrderSuspended(value) => ("long_entry_order_suspended", Some(value)),
        B::ShortEntryOrderSuspended(value) => ("short_entry_order_suspended", Some(value)),
        B::OrderAcceptanceTemporarilyRestricted(value) => {
            ("order_acceptance_temporarily_restricted", Some(value))
        }
        B::CannotWithdrawBecauseInformationRegistrationForUnreflectedDepositsHasNotBeCompleted(
            value,
        ) => ("unreflected_deposit_registration_incomplete", Some(value)),
    };
    let code = payload
        .and_then(|v| v.pointer("/data/code"))
        .and_then(serde_json::Value::as_u64);
    let security_relevant = matches!(
        error,
        B::AuthenticationFailed(_)
            | B::InvalidAccessKey(_)
            | B::AccessKeyNotFound(_)
            | B::AccessNonceNotFound(_)
            | B::InvalidAccessSignature(_)
            | B::MfaFailed(_)
            | B::SmsVerificationFailed(_)
            | B::PleaseLogin(_)
            | B::MissingOtpCode(_)
            | B::MissingSmsCode(_)
            | B::MissingOtpAndSmsCode(_)
            | B::MfaTemporarilyLocked(_)
            | B::MissingAccessRequestTime(_)
            | B::InvalidAccessRequestTime(_)
            | B::NoRequestSentWithinAccessTimeWindow(_)
            | B::AccessRequestTimeAndNonceNotFound(_)
            | B::InvalidAccessTimeWindow(_)
            | B::InvalidAccessNonce(_)
    );
    (reason, code, security_relevant)
}

pub(crate) fn venue(error: &TradingVenueError) -> (&'static str, Option<u64>, bool) {
    use crate::paper_execution::PaperExecutionError as P;
    let reason = match error {
        TradingVenueError::Numeric(e) => numeric(e),
        TradingVenueError::Parse(e) => parse(e),
        TradingVenueError::Bitbank(e) => return api(e),
        TradingVenueError::StatePoisoned => "state_poisoned",
        TradingVenueError::Unavailable => "unavailable",
        TradingVenueError::Paper(e) => match e {
            P::Numeric(e) => numeric(e),
            P::PostOnlyWouldTakeLiquidity => "post_only_would_take_liquidity",
            P::ExecutionModelUnavailable(_) => "execution_model_unavailable",
            P::InvalidClock => "invalid_clock",
            P::InvalidWatermark => "invalid_watermark",
            P::InvalidTradeStream => "invalid_trade_stream",
            P::PairMismatch { .. } => "pair_mismatch",
            P::NonPositiveOrder { .. } => "non_positive_order",
            P::InsufficientFunds { .. } => "insufficient_funds",
            P::MissingBalance(_) => "missing_balance",
            P::UnsupportedPair(_) => "unsupported_pair",
        },
    };
    (reason, None, false)
}
