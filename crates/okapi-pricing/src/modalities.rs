//! Independent cache/modality intersections, always relative to text input price.
use crate::{PricingError, RatioFp, ratio::RATIO_SCALE};
use okapi_domain::TokenUsage;
use serde::{Serialize, Serializer, ser::Error as _};
use serde_json::Value;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModalityRatios {
    pub image_cache_read: Option<RatioFp>,
    pub audio_cache_read: Option<RatioFp>,
    pub image_cache_write: Option<RatioFp>,
    pub audio_cache_write: Option<RatioFp>,
    pub image_output: Option<RatioFp>,
}

impl ModalityRatios {
    /// Strict configuration: decimal strings, no unknown axes or silently ignored prices.
    pub fn parse(value: &Value) -> Result<Self, &'static str> {
        let object = value.as_object().ok_or("modality_ratios")?;
        let mut rates = Self::default();
        for (name, value) in object {
            let rate = value
                .as_str()
                .ok_or("modality_ratios")?
                .parse()
                .map_err(|_| "modality_ratios")?;
            let target = match name.as_str() {
                "image_cache_read" => &mut rates.image_cache_read,
                "audio_cache_read" => &mut rates.audio_cache_read,
                "image_cache_write" => &mut rates.image_cache_write,
                "audio_cache_write" => &mut rates.audio_cache_write,
                "image_output" => &mut rates.image_output,
                _ => return Err("modality_ratios"),
            };
            *target = Some(rate);
        }
        Ok(rates)
    }

    /// Returns weighted tokens and only the effective rates actually used in this bill.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn charge(
        self,
        usage: TokenUsage,
        cache: RatioFp,
        write: RatioFp,
        audio: RatioFp,
        image: RatioFp,
        completion: RatioFp,
    ) -> Result<(i128, Self), PricingError> {
        let read = usage.cache_read_modalities.unwrap_or_default();
        let written = usage.cache_write_modalities.unwrap_or_default();
        let mut effective = Self::default();
        let mut total = 0_i128;
        for (count, explicit, base, fallback, target) in [
            (
                read.image_tokens,
                self.image_cache_read,
                image,
                cache,
                &mut effective.image_cache_read,
            ),
            (
                read.audio_tokens,
                self.audio_cache_read,
                audio,
                cache,
                &mut effective.audio_cache_read,
            ),
            (
                written.image_tokens,
                self.image_cache_write,
                image,
                write,
                &mut effective.image_cache_write,
            ),
            (
                written.audio_tokens,
                self.audio_cache_write,
                audio,
                write,
                &mut effective.audio_cache_write,
            ),
            (
                usage.image_completion_tokens,
                self.image_output,
                completion,
                RatioFp::ONE,
                &mut effective.image_output,
            ),
        ] {
            if count == 0 {
                continue;
            }
            let rate = match explicit {
                Some(rate) => rate,
                None => RatioFp::from_scaled(
                    i64::try_from(
                        i128::from(base.as_scaled()) * i128::from(fallback.as_scaled())
                            / i128::from(RATIO_SCALE),
                    )
                    .map_err(|_| PricingError::Overflow)?,
                )
                .ok_or(PricingError::Overflow)?,
            };
            total = total
                .checked_add(i128::from(count) * i128::from(rate.as_scaled()))
                .ok_or(PricingError::Overflow)?;
            *target = Some(rate);
        }
        Ok((total, effective))
    }
}

impl Serialize for ModalityRatios {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut values = serde_json::Map::new();
        for (name, rate) in [
            ("image_cache_read", self.image_cache_read),
            ("audio_cache_read", self.audio_cache_read),
            ("image_cache_write", self.image_cache_write),
            ("audio_cache_write", self.audio_cache_write),
            ("image_output", self.image_output),
        ] {
            if let Some(rate) = rate {
                values.insert(
                    name.into(),
                    Value::Number(
                        serde_json::Number::from_str(&rate.to_string())
                            .map_err(S::Error::custom)?,
                    ),
                );
            }
        }
        values.serialize(serializer)
    }
}
