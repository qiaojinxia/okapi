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
            Self::PerImage => previous
                .checked_add(next)
                .map(Some)
                .map_err(|_| usage::invalid()),
        }
    }
}
