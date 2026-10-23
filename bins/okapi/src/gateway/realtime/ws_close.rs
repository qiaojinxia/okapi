//! WebSocket 入口读到违规帧时回给客户端的关闭帧（RFC 6455 §7.4.1）。
//!
//! tungstenite 遇到协议违例只把错误交给上层，自己不发关闭帧；上层若直接 `close()`，对端收到的是
//! 没有状态码的空关闭——既看不出是自己的帧不合规（未掩码、保留位、帧 / 消息超限、非法 UTF-8），
//! 也分不清是不是被网关限流了。这里把读错误归类成标准状态码加一个短原因，让客户端自己能诊断。

use axum::extract::ws::{CloseFrame, close_code};
use tokio_tungstenite::tungstenite::Error as WsError;

fn classify(error: &WsError) -> Option<(u16, &'static str)> {
    match error {
        WsError::Capacity(_) => Some((close_code::SIZE, "message_too_big")),
        WsError::Utf8(_) => Some((close_code::INVALID, "invalid_utf8")),
        WsError::Protocol(_) => Some((close_code::PROTOCOL, "protocol_error")),
        // IO 断开、连接已关闭等：对端已经走了，没有可回的
        _ => None,
    }
}

/// 网关下线时回给客户端的关闭帧：1001 Going Away，客户端据此知道是服务端在重启、重连即可，
/// 而不是自己的帧有问题。
pub(crate) fn going_away() -> CloseFrame {
    CloseFrame {
        code: close_code::AWAY,
        reason: "gateway_shutting_down".into(),
    }
}

/// 客户端读流出错时该回的关闭帧；对端已断开之类没有可回的就是 `None`。
pub(crate) fn for_read_error(error: &axum::Error) -> Option<CloseFrame> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(ws) = current.downcast_ref::<WsError>() {
            return classify(ws).map(|(code, reason)| CloseFrame {
                code,
                reason: reason.into(),
            });
        }
        source = current.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::for_read_error;
    use tokio_tungstenite::tungstenite::{
        Error as WsError,
        error::{CapacityError, ProtocolError},
    };

    fn frame(error: WsError) -> Option<(u16, String)> {
        for_read_error(&axum::Error::new(error)).map(|f| (f.code, f.reason.to_string()))
    }

    #[test]
    fn protocol_violations_are_answered_with_standard_status_codes() {
        assert_eq!(
            frame(WsError::Protocol(ProtocolError::UnmaskedFrameFromClient)),
            Some((1002, "protocol_error".into()))
        );
        assert_eq!(
            frame(WsError::Protocol(ProtocolError::NonZeroReservedBits)),
            Some((1002, "protocol_error".into()))
        );
        assert_eq!(
            frame(WsError::Capacity(CapacityError::MessageTooLong {
                size: 30 << 20,
                max_size: 16 << 20,
            })),
            Some((1009, "message_too_big".into()))
        );
        assert_eq!(
            frame(WsError::Utf8("invalid utf-8 sequence".into())),
            Some((1007, "invalid_utf8".into()))
        );
    }

    #[test]
    fn a_vanished_peer_gets_no_reply() {
        let io = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        assert_eq!(frame(WsError::Io(io)), None);
        assert_eq!(frame(WsError::ConnectionClosed), None);
        assert_eq!(frame(WsError::AlreadyClosed), None);
        // 不是 tungstenite 的错误（比如上游换了实现）也不乱回
        assert!(for_read_error(&axum::Error::new("something else")).is_none());
    }

    #[test]
    fn shutdown_is_announced_as_going_away() {
        let frame = super::going_away();
        assert_eq!(
            (frame.code, frame.reason.to_string()),
            (1001, "gateway_shutting_down".to_owned())
        );
    }
}
