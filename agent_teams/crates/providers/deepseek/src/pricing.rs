//! 按实际发送模型的显式标准价和完整用量计算 USD，缺项保留未知

use gateway_core::metering::{
    CalculatedCost, CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates,
    CurrencyCode, Decimal, ModelPriceOverride, Money, Usage,
};

pub(crate) fn calculate(usage: &Usage, pricing: &ModelPriceOverride) -> Option<CalculatedCost> {
    let rates = pricing.bands.get("standard")?;
    let cached = usage.cached_input_tokens.unwrap_or(0);
    let written = usage.cache_write_input_tokens.unwrap_or(0);
    let input = usage
        .input_tokens?
        .checked_sub(cached)?
        .checked_sub(written)?;
    let amount = |tokens: u64, price: gateway_core::metering::TokenPrice| {
        u128::from(tokens).checked_mul(price.ticks_per_token())
    };
    let input = amount(input, rates.input)?;
    let output = amount(usage.output_tokens?, rates.output)?;
    let cached = amount(cached, rates.cache_read)?;
    let written = amount(written, rates.cache_write)?;
    let total = input
        .checked_add(output)?
        .checked_add(cached)?
        .checked_add(written)?;
    let per_million = |price: gateway_core::metering::TokenPrice| {
        usd(price.ticks_per_token().checked_mul(1_000_000)?)
    };
    Some(
        CalculatedCostBreakdown::new(
            CalculatedCostAmounts::new(
                usd(input)?,
                usd(output)?,
                usd(cached)?,
                usd(written)?,
                usd(total)?,
                usd(total)?,
            ),
            CalculatedCostRates::new(
                per_million(rates.input)?,
                per_million(rates.output)?,
                per_million(rates.cache_read)?,
                per_million(rates.cache_write)?,
            ),
            None,
            100,
        )
        .with_custom_multiplier(pricing.multiplier_bps)?
        .calculated_cost(),
    )
}

fn usd(ticks: u128) -> Option<Money> {
    Some(Money::new(
        Decimal::from_scaled(ticks).ok()?,
        CurrencyCode::new("USD").ok()?,
    ))
}
