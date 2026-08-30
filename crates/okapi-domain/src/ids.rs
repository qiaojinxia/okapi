//! ID 与代码类 newtype：防止裸 i64 / String 参数错位（rust-backend 规则）。

macro_rules! i64_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(i64);

        impl $name {
            #[must_use]
            pub const fn new(value: i64) -> Self {
                Self(value)
            }

            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

macro_rules! string_code {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

i64_id!(
    /// 用户 ID（users.id）。
    UserId
);
i64_id!(
    /// 渠道 ID（channels.id）。
    ChannelId
);
i64_id!(
    /// 渠道 key ID（channel_keys.id）。
    ChannelKeyId
);
i64_id!(
    /// API Key ID（api_keys.id）。
    ApiKeyId
);

string_code!(
    /// 对外模型名（models.model_name，canonical）。
    ModelCode
);
string_code!(
    /// 定价分组代码（price_groups.group_code）。
    GroupCode
);
