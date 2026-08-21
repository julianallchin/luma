//! Server-sent-event framing, shared by the two HTTP transports.
//!
//! Both providers stream `text/event-stream`; only the payload schema differs.
//! Framing, HTTP status handling and connection teardown are identical, so they
//! live here and each transport supplies just a [`SseParser`].

use futures_util::stream::{self, BoxStream};
use futures_util::{FutureExt, StreamExt};

use super::{ModelError, ModelEvent};

/// Turns one SSE `data:` payload into zero or more model events.
///
/// Implementations are stateful — a tool call's name arrives in one frame and
/// its arguments in later ones — which is why this is a trait object with
/// `&mut self` rather than a function.
pub(super) trait SseParser: Send + 'static {
    /// # Errors
    ///
    /// If the payload is not the shape this provider promised.
    fn event(&mut self, data: &str) -> Result<Vec<ModelEvent>, ModelError>;
}

/// `RequestBuilder` dwarfs the streaming state, so the connect phase is boxed:
/// every `unfold` step moves this value.
enum Phase<P> {
    Start(Box<(reqwest::RequestBuilder, P)>),
    Body {
        body: BoxStream<'static, reqwest::Result<Vec<u8>>>,
        buffer: String,
        parser: P,
    },
    Done,
}

type Emitted = Vec<Result<ModelEvent, ModelError>>;

/// Send `request` and stream its SSE payload through `parser`.
///
/// The returned stream is `'static` and owns the connection: dropping it
/// cancels the HTTP request, which is the whole cancellation story for a turn.
pub(super) fn stream_sse<P: SseParser>(
    provider: &'static str,
    request: reqwest::RequestBuilder,
    parser: P,
) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
    stream::unfold(
        Phase::Start(Box::new((request, parser))),
        move |phase| async move {
            match phase {
                Phase::Start(start) => {
                    let (request, parser) = *start;
                    Some(match connect(provider, request).await {
                        Ok(body) => (
                            Vec::new(),
                            Phase::Body {
                                body,
                                buffer: String::new(),
                                parser,
                            },
                        ),
                        Err(error) => (vec![Err(error)], Phase::Done),
                    })
                }
                Phase::Body {
                    mut body,
                    mut buffer,
                    mut parser,
                } => {
                    let chunk = body.next().await;
                    let Some(chunk) = chunk else {
                        // A clean end of body with a partial frame left over is the
                        // provider hanging up mid-message; the frame is discarded
                        // rather than guessed at.
                        return Some((Vec::new(), Phase::Done));
                    };
                    let chunk = match chunk {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            return Some((
                                vec![Err(ModelError::Transport(error.to_string()))],
                                Phase::Done,
                            ))
                        }
                    };
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    let mut emitted: Emitted = Vec::new();
                    let mut failed = false;
                    for frame in drain_frames(&mut buffer) {
                        let Some(data) = frame_data(&frame) else {
                            continue;
                        };
                        if data == "[DONE]" {
                            continue;
                        }
                        match parser.event(&data) {
                            Ok(events) => emitted.extend(events.into_iter().map(Ok)),
                            Err(error) => {
                                emitted.push(Err(error));
                                failed = true;
                                break;
                            }
                        }
                    }
                    Some((
                        emitted,
                        if failed {
                            Phase::Done
                        } else {
                            Phase::Body {
                                body,
                                buffer,
                                parser,
                            }
                        },
                    ))
                }
                Phase::Done => None,
            }
        },
    )
    .flat_map(stream::iter)
    .boxed()
}

async fn connect(
    provider: &'static str,
    request: reqwest::RequestBuilder,
) -> Result<BoxStream<'static, reqwest::Result<Vec<u8>>>, ModelError> {
    let response = request
        .send()
        .map(|result| result.map_err(|error| ModelError::Transport(error.to_string())))
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ModelError::Status {
            provider,
            status: status.as_u16(),
            body: truncate(&body, 2_000),
        });
    }
    Ok(response
        .bytes_stream()
        .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
        .boxed())
}

/// Split every complete frame out of `buffer`, leaving any partial tail.
fn drain_frames(buffer: &mut String) -> Vec<String> {
    let mut frames = Vec::new();
    loop {
        let Some((end, width)) = buffer
            .find("\n\n")
            .map(|at| (at, 2))
            .into_iter()
            .chain(buffer.find("\r\n\r\n").map(|at| (at, 4)))
            .min_by_key(|(at, _)| *at)
        else {
            return frames;
        };
        let frame = buffer[..end].to_string();
        buffer.drain(..end + width);
        frames.push(frame);
    }
}

/// The concatenated `data:` lines of one frame, or `None` for a frame that
/// carried only comments or metadata.
fn frame_data(frame: &str) -> Option<String> {
    let mut data = String::new();
    for line in frame.lines() {
        let Some(rest) = line.strip_prefix("data:") else {
            continue;
        };
        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
    }
    (!data.is_empty()).then_some(data)
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_split_on_blank_lines_and_keep_partial_tails() {
        let mut buffer =
            String::from("event: a\ndata: {\"x\":1}\n\ndata: one\ndata: two\n\ndata: part");
        let frames = drain_frames(&mut buffer);
        assert_eq!(frames.len(), 2);
        assert_eq!(frame_data(&frames[0]).as_deref(), Some("{\"x\":1}"));
        assert_eq!(frame_data(&frames[1]).as_deref(), Some("one\ntwo"));
        assert_eq!(buffer, "data: part");
    }
}
