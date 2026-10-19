//! MiMo 官方美元文本价目与缓存用量计算，套餐额度不参与本地价格推导

use gateway_core::metering::{
    CalculatedCost, CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates,
    CurrencyCode, Decimal, ModelPriceOverride, Money, Usage,
};
use std::collections::BTreeMap;

// 官方实时推理海外美元价，单位为每 Token 的 10^-10 USD
// https://mimo.mi.com/docs/zh-CN/price/pay-as-you-go
const PRICES: &[(&str, [u128; 3])] = &[
    ("mimo-v2.6-pro", [4350, 8700, 36]),
    ("mimo-v2.6-flash", [1400, 2800, 28]),
    ("mimo-v2.6-pro-ultraspeed", [43500, 87000, 360]),
];

pub(crate) fn catalog() -> BTreeMap<String, ModelPriceOverride> {
    PRICES
        .iter()
        .map(|(model, ticks)| ((*model).to_owned(), model_price(*ticks)))
        .collect()
}

pub(crate) fn resolve(
    model: &str,
    configured: Option<&ModelPriceOverride>,
) -> Option<ModelPriceOverride> {
    configured.cloned().or_else(|| {
        PRICES
            .iter()
            .find(|(name, _)| *name == model)
            .map(|(_, ticks)| model_price(*ticks))
    })
}

fn model_price(ticks: [u128; 3]) -> ModelPriceOverride {
    let price = |ticks: u128| {
        Decimal::from_scaled(ticks * 1_000_000)
            .expect("内置单价在范围内")
            .canonical()
            .try_into()
            .expect("内置单价精度合法")
    };
    ModelPriceOverride {
        multiplier_bps: 10_000,
        bands: BTreeMap::from([(
            "standard".to_owned(),
            gateway_core::metering::TokenPriceOverride {
                input: price(ticks[0]),
                output: price(ticks[1]),
                cache_read: price(ticks[2]),
                // 官方当前缓存写入免费，不套用其他 Provider 的输入价格
                cache_write: price(0),
            },
        )]),
    }
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
