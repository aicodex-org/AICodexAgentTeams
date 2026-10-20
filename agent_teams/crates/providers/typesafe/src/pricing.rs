//! Jev 已核实标准价与显式模型覆盖，缺失用量不冒充零费用

use gateway_core::metering::{
    CalculatedCost, CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates,
    CurrencyCode, Decimal, ModelPriceOverride, Money, Usage,
};
use std::collections::BTreeMap;

// https://docs.typesafe.ai/models.md，USD 每百万输入 0.042，输出为零
pub(crate) fn catalog() -> BTreeMap<String, ModelPriceOverride> {
    ["jev-latest", "jev-preview", "jev-1.13.0", "jev-1.14.0"]
        .into_iter()
        .map(|model| {
            (
                model.to_owned(),
                ModelPriceOverride {
                    multiplier_bps: 10_000,
                    bands: BTreeMap::from([(
                        "standard".to_owned(),
                        gateway_core::metering::TokenPriceOverride {
                            input: "0.042".to_owned().try_into().expect("内置单价格式合法"),
                            output: "0".to_owned().try_into().expect("内置单价格式合法"),
                            cache_read: "0.042".to_owned().try_into().expect("内置单价格式合法"),
                            cache_write: "0.042".to_owned().try_into().expect("内置单价格式合法"),
                        },
                    )]),
                },
            )
        })
        .collect()
}
pub(crate) fn resolve(
    model: &str,
    configured: Option<&ModelPriceOverride>,
) -> Option<ModelPriceOverride> {
    configured.cloned().or_else(|| catalog().remove(model))
}

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
    // 官方输出单价为零时，缺失输出计数不影响已知输入金额，但不能补写 Usage
    // 人工设置了非零输出价则仍要求上游提供输出计数
    let output_tokens = usage
        .output_tokens
        .or_else(|| (rates.output.ticks_per_token() == 0).then_some(0))?;
    let output = amount(output_tokens, rates.output)?;
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
