use super::{AppError, AppState, TokenUsage, usage};

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Cumulative,
    PerImage,
}

impl Mode {
    pub async fn load(state: &AppState, channel: i64) -> Result<Self, AppError> {
        let row = sqlx::query!(
            "SELECT provider, settings FROM channels WHERE id = $1 AND deleted_at IS NULL",
            channel
        )
        .fetch_one(&state.pg)
        .await
        .map_err(okapi_store::StoreError::from)?;
        match row.settings.get("image_stream_usage") {
            None => Ok(Self::Cumulative),
            Some(value) if value == "cumulative" => Ok(Self::Cumulative),
            Some(value) if value == "per_image" => Ok(Self::PerImage),
            Some(_) => Err(AppError::internal().with_param("image_stream_usage")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cumulative => "cumulative",
            Self::PerImage => "per_image",
        }
    }

    pub fn combine(
        self,
        previous: Option<TokenUsage>,
        next: Option<TokenUsage>,
    ) -> Result<Option<TokenUsage>, AppError> {
        let Some(next) = next else {
            return Ok(previous);
        };
        let Some(previous) = previous else {
            return Ok(Some(next));
        };
        match self {
            Self::Cumulative => {
                if next.prompt_uncached() < previous.prompt_uncached()
                    || next.image_prompt_tokens < previous.image_prompt_tokens
                    || next.text_completion() < previous.text_completion()
                    || next.image_completion_tokens < previous.image_completion_tokens
                    || next.cached_text() < previous.cached_text()
                    || next.cache_write_text() < previous.cache_write_text()
                    || next.cache_read_modalities.unwrap_or_default().image_tokens
                        < previous
                            .cache_read_modalities
                            .unwrap_or_default()
                            .image_tokens
                    || next.cache_write_modalities.unwrap_or_default().image_tokens
                        < previous
                            .cache_write_modalities
                            .unwrap_or_default()
                            .image_tokens
                {
                    return Err(usage::invalid());
                }
                Ok(Some(next))
            }
            Self::PerImage => {
                let add = |a: u32, b: u32| {
                    a.checked_add(b)
                        .filter(|value| i32::try_from(*value).is_ok())
                        .ok_or_else(usage::invalid)
                };
                let modalities =
                    |a: Option<okapi_domain::CacheModalities>,
                     b: Option<okapi_domain::CacheModalities>| {
                        if a.is_none() && b.is_none() {
                            return Ok::<_, AppError>(None);
                        }
                        Ok(Some(okapi_domain::CacheModalities {
                            audio_tokens: add(
                                a.unwrap_or_default().audio_tokens,
                                b.unwrap_or_default().audio_tokens,
                            )?,
                            image_tokens: add(
                                a.unwrap_or_default().image_tokens,
                                b.unwrap_or_default().image_tokens,
                            )?,
                        }))
                    };
                let combined = TokenUsage {
                    prompt_tokens: add(previous.prompt_tokens, next.prompt_tokens)?,
                    image_prompt_tokens: add(
                        previous.image_prompt_tokens,
                        next.image_prompt_tokens,
                    )?,
                    completion_tokens: add(previous.completion_tokens, next.completion_tokens)?,
                    image_completion_tokens: add(
                        previous.image_completion_tokens,
                        next.image_completion_tokens,
                    )?,
                    cached_tokens: add(previous.cached_tokens, next.cached_tokens)?,
                    cache_write_tokens: add(previous.cache_write_tokens, next.cache_write_tokens)?,
                    cache_read_modalities: modalities(
                        previous.cache_read_modalities,
                        next.cache_read_modalities,
                    )?,
                    cache_write_modalities: modalities(
                        previous.cache_write_modalities,
                        next.cache_write_modalities,
                    )?,
                    cache_read_reported: previous.cache_read_reported && next.cache_read_reported,
                    cache_write_reported: previous.cache_write_reported
                        && next.cache_write_reported,
                    ..TokenUsage::default()
                };
                combined.validate().map_err(|_| usage::invalid())?;
                Ok(Some(combined))
            }
        }
    }
}
